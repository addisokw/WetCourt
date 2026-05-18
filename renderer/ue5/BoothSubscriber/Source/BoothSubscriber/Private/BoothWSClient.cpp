// Copyright (c) WetCourt. All rights reserved.

#include "BoothWSClient.h"

#include "Dom/JsonObject.h"
#include "IWebSocket.h"
#include "Modules/ModuleManager.h"
#include "Serialization/JsonReader.h"
#include "Serialization/JsonSerializer.h"
#include "WebSocketsModule.h"

DEFINE_LOG_CATEGORY_STATIC(LogBoothWS, Log, All);

namespace
{
    constexpr float ReconnectStartSecs = 0.5f;
    constexpr float ReconnectMaxSecs   = 8.0f;
}

FBoothWSClient::FBoothWSClient() = default;

FBoothWSClient::~FBoothWSClient()
{
    Shutdown();
}

void FBoothWSClient::Initialize(const FString& InUrl)
{
    if (Socket.IsValid())
    {
        return;
    }
    Url = InUrl;
    bShuttingDown = false;
    NextReconnectDelaySecs = ReconnectStartSecs;

    if (!FModuleManager::Get().IsModuleLoaded(TEXT("WebSockets")))
    {
        FModuleManager::Get().LoadModule(TEXT("WebSockets"));
    }

    Connect();
}

void FBoothWSClient::Shutdown()
{
    bShuttingDown = true;
    if (ReconnectTickerHandle.IsValid())
    {
        FTSTicker::GetCoreTicker().RemoveTicker(ReconnectTickerHandle);
        ReconnectTickerHandle.Reset();
    }
    if (Socket.IsValid())
    {
        Socket->OnConnected().Clear();
        Socket->OnConnectionError().Clear();
        Socket->OnClosed().Clear();
        Socket->OnMessage().Clear();
        Socket->OnRawMessage().Clear();
        if (Socket->IsConnected())
        {
            Socket->Close();
        }
        Socket.Reset();
    }
    bConnected = false;
    bAudioStreaming = false;
    BinaryBuffer.Reset();
}

void FBoothWSClient::Connect()
{
    Socket = FWebSocketsModule::Get().CreateWebSocket(Url, TEXT("ws"));

    Socket->OnConnected().AddRaw(this, &FBoothWSClient::HandleConnected);
    Socket->OnConnectionError().AddRaw(this, &FBoothWSClient::HandleConnectionError);
    Socket->OnClosed().AddRaw(this, &FBoothWSClient::HandleClosed);
    Socket->OnMessage().AddRaw(this, &FBoothWSClient::HandleTextMessage);
    Socket->OnRawMessage().AddRaw(this, &FBoothWSClient::HandleRawMessage);

    UE_LOG(LogBoothWS, Log, TEXT("connecting to %s"), *Url);
    Socket->Connect();
}

void FBoothWSClient::HandleConnected()
{
    UE_LOG(LogBoothWS, Log, TEXT("connected"));
    bConnected = true;
    NextReconnectDelaySecs = ReconnectStartSecs;
    if (OnConnectionChanged)
    {
        OnConnectionChanged(true);
    }
    // Frontend convention — send a `ready` ClientEvent so the orchestrator
    // logs the subscription. Cheap, optional, no acknowledgement required.
    if (Socket.IsValid())
    {
        Socket->Send(TEXT("{\"type\":\"ready\"}"));
    }
}

void FBoothWSClient::HandleConnectionError(const FString& Error)
{
    UE_LOG(LogBoothWS, Warning, TEXT("connection error: %s"), *Error);
    bConnected = false;
    if (OnConnectionChanged)
    {
        OnConnectionChanged(false);
    }
    ScheduleReconnect();
}

void FBoothWSClient::HandleClosed(int32 StatusCode, const FString& Reason, bool bWasClean)
{
    UE_LOG(LogBoothWS, Log, TEXT("closed: code=%d reason=%s clean=%d"), StatusCode, *Reason, bWasClean ? 1 : 0);
    bConnected = false;
    bAudioStreaming = false;
    BinaryBuffer.Reset();
    if (OnConnectionChanged)
    {
        OnConnectionChanged(false);
    }
    if (!bShuttingDown)
    {
        ScheduleReconnect();
    }
}

void FBoothWSClient::HandleTextMessage(const FString& Message)
{
    TSharedPtr<FJsonObject> JsonObject;
    const TSharedRef<TJsonReader<>> Reader = TJsonReaderFactory<>::Create(Message);
    if (!FJsonSerializer::Deserialize(Reader, JsonObject) || !JsonObject.IsValid())
    {
        UE_LOG(LogBoothWS, Warning, TEXT("non-JSON text message: %s"), *Message.Left(80));
        return;
    }
    FString Type;
    if (!JsonObject->TryGetStringField(TEXT("type"), Type))
    {
        UE_LOG(LogBoothWS, Warning, TEXT("JSON missing `type` field: %s"), *Message.Left(80));
        return;
    }
    DispatchEvent(Type, JsonObject);
}

void FBoothWSClient::DispatchEvent(const FString& Type, const TSharedPtr<FJsonObject>& JsonObject)
{
    if (Type == TEXT("tts_audio"))
    {
        bAudioStreaming = true;
        BinaryBuffer.Reset();
        FString Format;
        JsonObject->TryGetStringField(TEXT("format"), Format);
        if (OnAudioSessionStart)
        {
            OnAudioSessionStart(Format);
        }
    }
    else if (Type == TEXT("tts_end"))
    {
        bAudioStreaming = false;
        if (OnAudioSessionEnd)
        {
            OnAudioSessionEnd();
        }
    }
    else if (Type == TEXT("idle") || Type == TEXT("reset"))
    {
        bAudioStreaming = false;
        BinaryBuffer.Reset();
    }

    if (OnDisplayEvent)
    {
        OnDisplayEvent(Type, JsonObject);
    }
}

void FBoothWSClient::HandleRawMessage(const void* Data, SIZE_T Size, SIZE_T BytesRemaining)
{
    if (!bAudioStreaming)
    {
        return; // binary outside an audio session — ignore
    }
    if (BinaryBuffer.Num() == 0 && BytesRemaining == 0)
    {
        // Hot path: whole message in one shot. Skip the buffer copy.
        if (OnAudioFrame && Size > 0)
        {
            OnAudioFrame(static_cast<const uint8*>(Data), static_cast<int32>(Size));
        }
        return;
    }
    BinaryBuffer.Append(static_cast<const uint8*>(Data), static_cast<int32>(Size));
    if (BytesRemaining == 0)
    {
        if (OnAudioFrame && BinaryBuffer.Num() > 0)
        {
            OnAudioFrame(BinaryBuffer.GetData(), BinaryBuffer.Num());
        }
        BinaryBuffer.Reset();
    }
}

void FBoothWSClient::ScheduleReconnect()
{
    if (bShuttingDown || ReconnectTickerHandle.IsValid())
    {
        return;
    }
    if (Socket.IsValid())
    {
        Socket->OnConnected().Clear();
        Socket->OnConnectionError().Clear();
        Socket->OnClosed().Clear();
        Socket->OnMessage().Clear();
        Socket->OnRawMessage().Clear();
        Socket.Reset();
    }
    ReconnectAccumSecs = 0.0f;
    ReconnectAtSecs = NextReconnectDelaySecs;
    UE_LOG(LogBoothWS, Log, TEXT("scheduling reconnect in %.1f s"), ReconnectAtSecs);
    NextReconnectDelaySecs = FMath::Min(NextReconnectDelaySecs * 2.0f, ReconnectMaxSecs);
    ReconnectTickerHandle = FTSTicker::GetCoreTicker().AddTicker(
        FTickerDelegate::CreateRaw(this, &FBoothWSClient::TickReconnect), 0.25f);
}

bool FBoothWSClient::TickReconnect(float DeltaTime)
{
    ReconnectAccumSecs += DeltaTime;
    if (ReconnectAccumSecs < ReconnectAtSecs)
    {
        return true; // keep ticking
    }
    ReconnectTickerHandle.Reset();
    if (!bShuttingDown)
    {
        Connect();
    }
    return false; // stop this ticker; Connect() either succeeds or fails into another ScheduleReconnect
}

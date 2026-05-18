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
    // UE 5.7's libwebsockets backend doesn't dispatch OnBinaryMessage in
    // practice — every message lands on OnRawMessage regardless of opcode.
    // We filter text vs. binary by content: text frames start with `{"type":`.
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

bool FBoothWSClient::LooksLikeJsonEvent(const uint8* Data, SIZE_T Size)
{
    // The orchestrator's DisplayEvent enum serializes as `{"type":"..."}`
    // with the `type` discriminator first. Random PCM s16le bytes matching
    // this 8-character prefix in the right positions is statistically zero
    // (~1 in 2^64 per frame), so this filter is safe for separating text
    // and binary on UE 5.7's libwebsockets backend (which broadcasts both
    // to OnRawMessage).
    if (Size < 8 || Data[0] != '{')
    {
        return false;
    }
    const SIZE_T MaxScan = FMath::Min<SIZE_T>(Size - 6, 16);
    for (SIZE_T i = 1; i <= MaxScan; ++i)
    {
        if (Data[i]   == '"' && Data[i+1] == 't' && Data[i+2] == 'y' &&
            Data[i+3] == 'p' && Data[i+4] == 'e' && Data[i+5] == '"')
        {
            return true;
        }
    }
    return false;
}

void FBoothWSClient::HandleRawMessage(const void* Data, SIZE_T Size, SIZE_T BytesRemaining)
{
    const uint8* Bytes = static_cast<const uint8*>(Data);
    if (BinaryBuffer.Num() == 0 && LooksLikeJsonEvent(Bytes, Size))
    {
        // Text frame — OnMessage will (or has) handled it. Skip the raw
        // echo so the JSON bytes don't get pipelined into the audio queue.
        return;
    }
    UE_LOG(LogBoothWS, Verbose, TEXT("raw binary: size=%llu remaining=%llu"),
        static_cast<uint64>(Size), static_cast<uint64>(BytesRemaining));

    // Note: no `bAudioStreaming` gate. In UE 5.7's libwebsockets backend,
    // all text events for a tick are dispatched before any binary, so
    // tts_end always lands before its preceding audio bytes. Gating on
    // tts_audio/tts_end would drop every byte of PCM. The actor handles
    // session boundaries (resampler reset, preroll) on the JSON timeline
    // independently; audio just streams into the procedural wave as it
    // arrives.

    if (BinaryBuffer.Num() == 0 && BytesRemaining == 0)
    {
        // Hot path: whole message in one shot. Skip the buffer copy.
        if (OnAudioFrame && Size > 0)
        {
            OnAudioFrame(Bytes, static_cast<int32>(Size));
        }
        return;
    }
    BinaryBuffer.Append(Bytes, static_cast<int32>(Size));
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

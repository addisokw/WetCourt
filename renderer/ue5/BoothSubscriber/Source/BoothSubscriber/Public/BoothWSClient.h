// Copyright (c) WetCourt. All rights reserved.

#pragma once

#include "CoreMinimal.h"
#include "Containers/Ticker.h"

class IWebSocket;
class FJsonObject;

/**
 * WebSocket client for the WetCourt orchestrator's /ws endpoint.
 *
 * Parses the DisplayEvent JSON enum (snake_case `type` field; see
 * orchestrator/src/display/events.rs for the full set) and forwards
 * binary audio frames between `tts_audio` and `tts_end` boundaries via
 * the OnAudioFrame callback.
 *
 * Reconnects on close with exponential backoff (500 ms -> 8 s cap), the
 * same shape as the operator browser frontend.
 */
class BOOTHSUBSCRIBER_API FBoothWSClient
{
public:
    FBoothWSClient();
    ~FBoothWSClient();

    /** Bind callbacks BEFORE calling Initialize. All fire on the game thread. */
    TFunction<void(const FString& /*Format*/)> OnAudioSessionStart;
    TFunction<void(const uint8* /*Data*/, int32 /*Size*/)> OnAudioFrame;
    TFunction<void()> OnAudioSessionEnd;
    /**
     * Per-utterance emotion vector emitted by the LLM stage right before the
     * next `tts_audio` event. Keys are lowercased A2F-3D emotion names
     * (anger, joy, disgust, sadness, ...); values 0..1. OverallStrength /
     * OverrideStrength map to FAudio2FaceEmotion's same-named fields.
     */
    TFunction<void(const TMap<FString, float>& /*Emotions*/,
                   float /*OverallStrength*/,
                   float /*OverrideStrength*/)> OnTtsEmotion;
    TFunction<void(const FString& /*Type*/, const TSharedPtr<FJsonObject>& /*Event*/)> OnDisplayEvent;
    TFunction<void(bool /*bConnected*/)> OnConnectionChanged;

    /** Open the socket. Idempotent — calling twice is a no-op. */
    void Initialize(const FString& Url);

    /** Close + cancel any pending reconnect. */
    void Shutdown();

    bool IsConnected() const { return bConnected; }

private:
    void Connect();
    void HandleConnected();
    void HandleConnectionError(const FString& Error);
    void HandleClosed(int32 StatusCode, const FString& Reason, bool bWasClean);
    void HandleTextMessage(const FString& Message);
    void HandleRawMessage(const void* Data, SIZE_T Size, SIZE_T BytesRemaining);
    static bool LooksLikeJsonEvent(const uint8* Data, SIZE_T Size);

    void ScheduleReconnect();
    bool TickReconnect(float DeltaTime);

    void DispatchEvent(const FString& Type, const TSharedPtr<FJsonObject>& JsonObject);

    TSharedPtr<IWebSocket> Socket;
    FString Url;
    bool bConnected = false;
    bool bShuttingDown = false;
    // Gate: binary frames are routed to OnAudioFrame only between
    // `tts_audio` and the next `tts_end`. Anything else is ignored.
    bool bAudioStreaming = false;
    // Defensive buffer in case the orchestrator ever splits a binary
    // message across multiple WS frames (it currently doesn't).
    TArray<uint8> BinaryBuffer;

    float NextReconnectDelaySecs = 0.5f;
    FTSTicker::FDelegateHandle ReconnectTickerHandle;
    float ReconnectAccumSecs = 0.0f;
    float ReconnectAtSecs = 0.0f;
};

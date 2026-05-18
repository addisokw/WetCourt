// Copyright (c) WetCourt. All rights reserved.

#pragma once

#include "BoothWSClient.h"
#include "CoreMinimal.h"
#include "GameFramework/Actor.h"
#include "Resample24To16.h"

#include "BoothFaceActor.generated.h"

class UAudioComponent;
class USoundWaveProcedural;

/**
 * Actor that subscribes to the WetCourt orchestrator and drives a
 * MetaHuman face.
 *
 * Three streams flow on each TTS utterance:
 *
 *   1. Orchestrator -> WS binary (24 kHz s16le mono).
 *   2. Resampled to 16 kHz -> fed to NVIDIA ACE A2F-3D NIM for blendshapes.
 *   3. Original 24 kHz queued to a USoundWaveProcedural for booth speaker
 *      output, delayed by ~p99 NIM latency so blendshapes arrive in time.
 *
 * Place one of these in the booth scene and parent a MetaHuman to it (or
 * reference the MetaHuman's face anim BP and wire ACE Face Animation node).
 *
 * The ACE plugin calls are wrapped in `#if WITH_ACE_RUNTIME` and disabled
 * by default; enable on the renderer PC once the NVIDIA ACE Unreal Plugin
 * is installed and its module is added to BoothSubscriber.Build.cs.
 */
UCLASS()
class BOOTHSUBSCRIBER_API ABoothFaceActor : public AActor
{
    GENERATED_BODY()

public:
    ABoothFaceActor();

    /** ws:// URL of the orchestrator. Default targets the Spark LAN address. */
    UPROPERTY(EditAnywhere, BlueprintReadOnly, Category = "Booth")
    FString OrchestratorWsUrl = TEXT("ws://10.10.1.221:8080/ws");

    /**
     * Hold each session's first audio for this long before starting
     * playback, so A2F blendshapes have arrived by then. p99 NIM latency
     * was measured at ~32 ms; 50 ms is a safe production value.
     */
    UPROPERTY(EditAnywhere, BlueprintReadOnly, Category = "Booth", meta=(ClampMin="0.0", ClampMax="0.5"))
    float AudioPlaybackDelaySecs = 0.05f;

    /** Disable audio playback (e.g. when this box is dev-only, not booth). */
    UPROPERTY(EditAnywhere, BlueprintReadOnly, Category = "Booth")
    bool bMuteAudio = false;

protected:
    virtual void BeginPlay() override;
    virtual void EndPlay(const EEndPlayReason::Type EndPlayReason) override;

private:
    void HandleAudioSessionStart(const FString& Format);
    void HandleAudioFrame(const uint8* Data, int32 Size);
    void HandleAudioSessionEnd();
    void HandleDisplayEvent(const FString& Type, const TSharedPtr<FJsonObject>& Event);
    void HandleConnectionChanged(bool bConnected);

    void FlushBufferedAudio();

    UPROPERTY()
    TObjectPtr<USoundWaveProcedural> ProceduralWave;

    UPROPERTY()
    TObjectPtr<UAudioComponent> AudioComponent;

    TUniquePtr<FBoothWSClient> WSClient;
    FResample24To16 Resampler;

    // Buffer of aligned bytes received during the playback preroll window;
    // flushed in one shot at delay+0, then bypassed.
    TArray<uint8> SessionStartBuffer;
    bool bSessionPrerollActive = false;
    FTimerHandle PrerollTimer;

    // Carries 0 or 1 byte across WS frames so int16-stride alignment holds
    // across arbitrarily chunked PCM (the orchestrator's last chunk per
    // session is frequently odd-byte). Mirrors the operator browser's
    // pcmResidue in orchestrator/frontend/src/audio.ts.
    TArray<uint8> AudioResidue;
};

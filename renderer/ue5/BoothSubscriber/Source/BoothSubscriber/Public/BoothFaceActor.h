// Copyright (c) WetCourt. All rights reserved.

#pragma once

#include "BoothWSClient.h"
#include "CoreMinimal.h"
#include "GameFramework/Actor.h"
#include "Resample24To16.h"

#include "BoothFaceActor.generated.h"

class UACEAudioCurveSourceComponent;

/**
 * Actor that subscribes to the WetCourt orchestrator and drives a
 * MetaHuman face via NVIDIA ACE Audio2Face-3D.
 *
 * Per utterance:
 *   1. Orchestrator -> WS binary frames (24 kHz s16le mono).
 *   2. Resampled to 16 kHz -> NVIDIA ACE A2F-3D NIM (gRPC) for blendshapes.
 *   3. Original 24 kHz passthrough -> ACE plays it synced with the curves.
 *
 * The actor owns a UACEAudioCurveSourceComponent at its root; ACE feeds
 * audio and blendshape curves into that component. Wire a MetaHuman's
 * face anim BP to the "Apply ACE Face Animation" anim node referencing
 * this actor's curve source for lipsync.
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

    /** Mute the ACE curve source's audio output (e.g. dev box, not booth). */
    UPROPERTY(EditAnywhere, BlueprintReadOnly, Category = "Booth")
    bool bMuteAudio = false;

    /**
     * Audio2Face-3D server URL. Default points at the A2F-3D NIM running
     * on this same box (Strix-4070 dev setup). The NIM listens on 52000.
     * Override per-instance for production / remote NIM hosts.
     */
    UPROPERTY(EditAnywhere, BlueprintReadOnly, Category = "Booth|ACE")
    FString A2FUrl = TEXT("http://localhost:52000");

    /**
     * Audio2Face-3D provider name. The remote-via-gRPC provider that
     * ships with the NV_ACE_Reference plugin registers as "RemoteA2F".
     */
    UPROPERTY(EditAnywhere, BlueprintReadOnly, Category = "Booth|ACE")
    FName A2FProviderName = FName(TEXT("RemoteA2F"));

protected:
    virtual void BeginPlay() override;
    virtual void EndPlay(const EEndPlayReason::Type EndPlayReason) override;

private:
    void HandleAudioSessionStart(const FString& Format);
    void HandleAudioFrame(const uint8* Data, int32 Size);
    void HandleAudioSessionEnd();
    void HandleDisplayEvent(const FString& Type, const TSharedPtr<FJsonObject>& Event);
    void HandleConnectionChanged(bool bConnected);

    TUniquePtr<FBoothWSClient> WSClient;
    FResample24To16 Resampler;

    // Carries 0 or 1 byte across WS frames so int16-stride alignment holds
    // across arbitrarily chunked PCM (the orchestrator's last chunk per
    // session is frequently odd-byte). Mirrors the operator browser's
    // pcmResidue in orchestrator/frontend/src/audio.ts.
    TArray<uint8> AudioResidue;

    // Curve-source component receives audio + blendshape curves from the
    // ACE provider; doubles as the root SceneComponent. Replace later
    // with a MetaHuman that has the "Apply ACE Face Animation" anim node
    // referencing this component for end-to-end lipsync.
    UPROPERTY()
    TObjectPtr<UACEAudioCurveSourceComponent> AceCurveSource;

    // Current open A2F-3D stream (lifecycle: tts_audio -> CreateA2FStream,
    // tts_end -> EndOutgoingStream). Plugin-owned IA2FProvider::IA2FStream*;
    // stored as void* so the header doesn't need to include A2FProvider.h.
    void* AceStreamPtr = nullptr;
};

// Copyright (c) WetCourt. All rights reserved.

#include "BoothFaceActor.h"

#include "Dom/JsonObject.h"
#include "Engine/World.h"

#if WITH_ACE_RUNTIME
#include "A2FProvider.h"
#include "ACEAudioCurveSourceComponent.h"
#include "ACEBlueprintLibrary.h"
#include "ACETypes.h"
#endif

DEFINE_LOG_CATEGORY_STATIC(LogBoothFace, Log, All);

namespace
{
    constexpr int32 TtsSampleRate = 24000;
    constexpr int32 AceSampleRate = 16000;
    constexpr int32 NumChannels = 1;
}

ABoothFaceActor::ABoothFaceActor()
{
    PrimaryActorTick.bCanEverTick = false;
}

void ABoothFaceActor::BeginPlay()
{
    Super::BeginPlay();

#if WITH_ACE_RUNTIME
    // Curve-source component plays the orchestrator's audio (via its
    // internal AudioComponent) AND surfaces blendshape curves for an
    // anim BP to consume. Attached as the actor root so it has a world
    // transform — required by SceneComponent base class even though we
    // don't use 3D positioning here.
    AceCurveSource = NewObject<UACEAudioCurveSourceComponent>(this, TEXT("AceCurveSource"));
    SetRootComponent(AceCurveSource);
    AceCurveSource->RegisterComponent();
    AceCurveSource->Volume = bMuteAudio ? 0.0f : 1.0f;

    // Configure the gRPC endpoint for the A2F-3D NIM.
    FACEConnectionInfo ConnectionInfo;
    ConnectionInfo.DestURL = A2FUrl;
    UACEBlueprintLibrary::SetA2XConnectionInfo(ConnectionInfo, A2FProviderName);

    // Pre-warm the gRPC connection so the first utterance's tts_end
    // doesn't beat the connection establishment. Without this, SID 0
    // closes its stream before any audio is sent and the NIM logs
    // "Audio2Face inference instance not provided".
    UACEBlueprintLibrary::AllocateA2F3DResources(A2FProviderName);

    UE_LOG(LogBoothFace, Log, TEXT("ACE: configured provider %s -> %s (pre-warmed)"),
        *A2FProviderName.ToString(), *A2FUrl);
#endif

    WSClient = MakeUnique<FBoothWSClient>();
    WSClient->OnAudioSessionStart = [this](const FString& Fmt) { HandleAudioSessionStart(Fmt); };
    WSClient->OnAudioFrame        = [this](const uint8* D, int32 N) { HandleAudioFrame(D, N); };
    WSClient->OnAudioSessionEnd   = [this]() { HandleAudioSessionEnd(); };
    WSClient->OnDisplayEvent      = [this](const FString& T, const TSharedPtr<FJsonObject>& E) { HandleDisplayEvent(T, E); };
    WSClient->OnConnectionChanged = [this](bool bConn) { HandleConnectionChanged(bConn); };
    WSClient->Initialize(OrchestratorWsUrl);
}

void ABoothFaceActor::EndPlay(const EEndPlayReason::Type EndPlayReason)
{
    if (WSClient.IsValid())
    {
        WSClient->Shutdown();
        WSClient.Reset();
    }
#if WITH_ACE_RUNTIME
    if (AceStreamPtr)
    {
        if (IA2FProvider* Provider = IA2FProvider::FindProvider(A2FProviderName))
        {
            Provider->EndOutgoingStream(static_cast<IA2FProvider::IA2FStream*>(AceStreamPtr));
        }
        AceStreamPtr = nullptr;
    }
    if (AceCurveSource)
    {
        AceCurveSource->Stop();
    }
#endif
    Super::EndPlay(EndPlayReason);
}

void ABoothFaceActor::HandleAudioSessionStart(const FString& Format)
{
    UE_LOG(LogBoothFace, Log, TEXT("audio session start: format=%s"), *Format);
    Resampler.Reset();
    AudioResidue.Reset();

#if WITH_ACE_RUNTIME
    // Close any straggler stream from the previous utterance before
    // opening a new one. (Idempotent if AceStreamPtr is already null.)
    if (AceStreamPtr)
    {
        if (IA2FProvider* Provider = IA2FProvider::FindProvider(A2FProviderName))
        {
            Provider->EndOutgoingStream(static_cast<IA2FProvider::IA2FStream*>(AceStreamPtr));
        }
        AceStreamPtr = nullptr;
    }
    if (IA2FProvider* Provider = IA2FProvider::FindProvider(A2FProviderName))
    {
        if (AceCurveSource)
        {
            AceStreamPtr = Provider->CreateA2FStream(AceCurveSource);
        }
        if (!AceStreamPtr)
        {
            UE_LOG(LogBoothFace, Warning, TEXT("ACE: CreateA2FStream returned null (provider=%s)"),
                *A2FProviderName.ToString());
        }
        else if (IA2FPassthroughProvider* Passthrough = Provider->GetAudioPassthroughProvider())
        {
            // Tell ACE about the *original* audio format so it can play
            // the orchestrator's 24 kHz audio alongside the blendshapes
            // (without the resampler-degraded 16 kHz copy we feed to A2F).
            Passthrough->SetOriginalAudioParams(
                static_cast<IA2FProvider::IA2FStream*>(AceStreamPtr),
                /*SampleRate=*/ TtsSampleRate,
                /*NumChannels=*/ NumChannels,
                /*SampleByteSize=*/ 2);
        }
    }
    else
    {
        UE_LOG(LogBoothFace, Warning, TEXT("ACE: provider %s not registered"),
            *A2FProviderName.ToString());
    }
#endif
}

void ABoothFaceActor::HandleAudioFrame(const uint8* Data, int32 Size)
{
    if (Size <= 0)
    {
        return;
    }

    // Concatenate any prior odd-byte residue with this frame, then split
    // into an int16-aligned prefix + (optional) one-byte residue carried
    // to the next call. Orchestrator emits arbitrary-byte chunks; the
    // tail per session is frequently odd-byte.
    TArray<uint8> Aligned;
    Aligned.Reserve(AudioResidue.Num() + Size);
    Aligned.Append(AudioResidue);
    Aligned.Append(Data, Size);
    AudioResidue.Reset();

    const int32 EvenLen = Aligned.Num() & ~1;
    if (EvenLen == 0)
    {
        AudioResidue = MoveTemp(Aligned);
        return;
    }
    if (Aligned.Num() & 1)
    {
        AudioResidue.Add(Aligned[Aligned.Num() - 1]);
    }

    // Resample 24 -> 16 kHz for A2F input.
    const int32 NumSamples24k = EvenLen / 2;
    const int16* Samples24k = reinterpret_cast<const int16*>(Aligned.GetData());
    TArray<int16> Samples16k;
    Samples16k.Reserve((NumSamples24k * 2) / 3 + 4);
    Resampler.Process(Samples24k, NumSamples24k, Samples16k);

#if WITH_ACE_RUNTIME
    if (AceStreamPtr)
    {
        if (IA2FProvider* Provider = IA2FProvider::FindProvider(A2FProviderName))
        {
            auto* Stream = static_cast<IA2FProvider::IA2FStream*>(AceStreamPtr);
            Provider->SendAudioSamples(
                Stream,
                TArrayView<const int16>(Samples16k.GetData(), Samples16k.Num()),
                /*EmotionParameters=*/ TOptional<FAudio2FaceEmotion>{},
                /*Audio2FaceParameters=*/ nullptr);
            // Pass the original 24 kHz audio through so ACE plays it
            // (no resampler artifacts) in sync with the blendshapes.
            if (IA2FPassthroughProvider* Passthrough = Provider->GetAudioPassthroughProvider())
            {
                Passthrough->EnqueueOriginalSamples(
                    Stream,
                    TArrayView<const uint8>(Aligned.GetData(), EvenLen));
            }
        }
    }
#else
    (void)Samples16k;
#endif
}

void ABoothFaceActor::HandleAudioSessionEnd()
{
    UE_LOG(LogBoothFace, Log, TEXT("audio session end"));

#if WITH_ACE_RUNTIME
    if (AceStreamPtr)
    {
        if (IA2FProvider* Provider = IA2FProvider::FindProvider(A2FProviderName))
        {
            Provider->EndOutgoingStream(static_cast<IA2FProvider::IA2FStream*>(AceStreamPtr));
        }
        AceStreamPtr = nullptr;
    }
#endif
}

void ABoothFaceActor::HandleDisplayEvent(const FString& Type, const TSharedPtr<FJsonObject>& Event)
{
    UE_LOG(LogBoothFace, Verbose, TEXT("event: %s"), *Type);
}

void ABoothFaceActor::HandleConnectionChanged(bool bConnected)
{
    UE_LOG(LogBoothFace, Log, TEXT("ws %s"), bConnected ? TEXT("connected") : TEXT("disconnected"));
}

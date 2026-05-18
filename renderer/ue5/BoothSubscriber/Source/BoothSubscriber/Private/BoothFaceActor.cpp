// Copyright (c) WetCourt. All rights reserved.

#include "BoothFaceActor.h"

#include "Components/AudioComponent.h"
#include "Dom/JsonObject.h"
#include "Engine/World.h"
#include "Sound/SoundWaveProcedural.h"
#include "TimerManager.h"

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

    ProceduralWave = NewObject<USoundWaveProcedural>(this);
    ProceduralWave->SetSampleRate(TtsSampleRate);
    ProceduralWave->NumChannels = NumChannels;
    ProceduralWave->Duration = INDEFINITELY_LOOPING_DURATION;
    ProceduralWave->SoundGroup = SOUNDGROUP_Voice;
    ProceduralWave->bLooping = false;

    AudioComponent = NewObject<UAudioComponent>(this);
    AudioComponent->bAutoActivate = false;
    AudioComponent->SetSound(ProceduralWave);
    AudioComponent->RegisterComponent();
    if (!bMuteAudio)
    {
        AudioComponent->Play();
    }

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
    if (PrerollTimer.IsValid() && GetWorldTimerManager().IsTimerActive(PrerollTimer))
    {
        GetWorldTimerManager().ClearTimer(PrerollTimer);
    }
    if (WSClient.IsValid())
    {
        WSClient->Shutdown();
        WSClient.Reset();
    }
    if (AudioComponent)
    {
        AudioComponent->Stop();
    }
    Super::EndPlay(EndPlayReason);
}

void ABoothFaceActor::HandleAudioSessionStart(const FString& Format)
{
    UE_LOG(LogBoothFace, Log, TEXT("audio session start: format=%s"), *Format);
    Resampler.Reset();
    SessionStartBuffer.Reset();
    bSessionPrerollActive = AudioPlaybackDelaySecs > 0.0f;

#if WITH_ACE_RUNTIME
    // TODO(renderer-pc): open a new ACE A2F-3D streaming session here.
    // Planned shape per the plan:
    //   FACERuntimeModule::Get().BeginAudioSession(/*sampleRate=*/ AceSampleRate);
#endif

    if (bSessionPrerollActive)
    {
        GetWorldTimerManager().SetTimer(
            PrerollTimer,
            FTimerDelegate::CreateUObject(this, &ABoothFaceActor::FlushBufferedAudio),
            AudioPlaybackDelaySecs,
            /*bLoop=*/ false);
    }
}

void ABoothFaceActor::HandleAudioFrame(const uint8* Data, int32 Size)
{
    if (Size <= 0)
    {
        return;
    }

    // Branch 1 — resample to 16 kHz and feed to A2F for blendshapes.
    if (Size % 2 != 0)
    {
        UE_LOG(LogBoothFace, Warning, TEXT("odd-byte audio frame (%d); dropping last byte"), Size);
    }
    const int32 NumSamples24k = Size / 2;
    const int16* Samples24k = reinterpret_cast<const int16*>(Data);

    TArray<int16> Samples16k;
    Samples16k.Reserve((NumSamples24k * 2) / 3 + 4);
    Resampler.Process(Samples24k, NumSamples24k, Samples16k);

#if WITH_ACE_RUNTIME
    // TODO(renderer-pc): feed to A2F. Planned shape per the plan:
    //   FACERuntimeModule::Get().AnimateFromAudioSamples(
    //       MakeArrayView(Samples16k.GetData(), Samples16k.Num()),
    //       /*bEndOfSamples=*/ false);
    (void)Samples16k;
#else
    (void)Samples16k;
#endif

    // Branch 2 — original 24 kHz PCM to procedural sound, with preroll
    // hold so A2F blendshapes have a head start.
    if (bMuteAudio || !ProceduralWave)
    {
        return;
    }
    if (bSessionPrerollActive)
    {
        SessionStartBuffer.Append(Data, Size);
    }
    else
    {
        ProceduralWave->QueueAudio(Data, Size);
    }
}

void ABoothFaceActor::HandleAudioSessionEnd()
{
    UE_LOG(LogBoothFace, Log, TEXT("audio session end"));

#if WITH_ACE_RUNTIME
    // TODO(renderer-pc): signal end-of-samples so A2F flushes its tail.
    //   FACERuntimeModule::Get().AnimateFromAudioSamples(TArrayView<const int16>(), /*bEndOfSamples=*/ true);
#endif

    // If the session ended before the preroll timer fired (very short
    // utterances), play out what we buffered now.
    if (bSessionPrerollActive)
    {
        FlushBufferedAudio();
        if (PrerollTimer.IsValid() && GetWorldTimerManager().IsTimerActive(PrerollTimer))
        {
            GetWorldTimerManager().ClearTimer(PrerollTimer);
        }
    }
}

void ABoothFaceActor::FlushBufferedAudio()
{
    bSessionPrerollActive = false;
    if (ProceduralWave && !bMuteAudio && SessionStartBuffer.Num() > 0)
    {
        ProceduralWave->QueueAudio(SessionStartBuffer.GetData(), SessionStartBuffer.Num());
    }
    SessionStartBuffer.Reset();
}

void ABoothFaceActor::HandleDisplayEvent(const FString& Type, const TSharedPtr<FJsonObject>& Event)
{
    // Generic state-display hook. Useful for diagnostics + driving non-face
    // visuals (e.g. lighting cues) from the same WS feed. Leaving as a
    // verbose log for now.
    UE_LOG(LogBoothFace, Verbose, TEXT("event: %s"), *Type);
}

void ABoothFaceActor::HandleConnectionChanged(bool bConnected)
{
    UE_LOG(LogBoothFace, Log, TEXT("ws %s"), bConnected ? TEXT("connected") : TEXT("disconnected"));
}

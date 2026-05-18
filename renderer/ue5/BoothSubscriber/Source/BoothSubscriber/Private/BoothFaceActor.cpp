// Copyright (c) WetCourt. All rights reserved.

#include "BoothFaceActor.h"

#include "Components/AudioComponent.h"
#include "Dom/JsonObject.h"
#include "Engine/World.h"
#include "Kismet/GameplayStatics.h"
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
    ProceduralWave->SoundGroup = SOUNDGROUP_Default;
    // Keep the audio engine pulling from our queue even when it's
    // momentarily empty between TTS chunks; without this the wave can
    // finish on the first underrun and we never recover.
    ProceduralWave->bLooping = true;
    ProceduralWave->Pitch = 1.0f;
    ProceduralWave->Volume = 1.0f;

    if (!bMuteAudio)
    {
        // UGameplayStatics::SpawnSound2D is the canonical UE 5 path for a
        // procedural / streaming 2D sound — it auto-registers with the
        // audio mixer, sets up the component correctly, and avoids the
        // "wave was created but the mixer never picked it up" failure
        // mode we saw with manual NewObject<UAudioComponent>. We keep a
        // reference so we can Stop() in EndPlay.
        AudioComponent = UGameplayStatics::SpawnSound2D(
            this,
            ProceduralWave,
            /*VolumeMultiplier=*/ 1.0f,
            /*PitchMultiplier=*/ 1.0f,
            /*StartTime=*/ 0.0f,
            /*ConcurrencySettings=*/ nullptr,
            /*bPersistAcrossLevelTransition=*/ false,
            /*bAutoDestroy=*/ false);
        if (AudioComponent)
        {
            AudioComponent->bAllowSpatialization = false;
            AudioComponent->bIsUISound = true;
            UE_LOG(LogBoothFace, Log, TEXT("audio component spawned (2D, non-spatial)"));
        }
        else
        {
            UE_LOG(LogBoothFace, Warning, TEXT("SpawnSound2D returned null"));
        }
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
    const int32 RemainingInQueue = ProceduralWave ? ProceduralWave->GetAvailableAudioByteCount() : 0;
    UE_LOG(LogBoothFace, Log, TEXT("audio session start: format=%s, queue=%d bytes"), *Format, RemainingInQueue);
    Resampler.Reset();
    SessionStartBuffer.Reset();
    AudioResidue.Reset();
    bSessionPrerollActive = AudioPlaybackDelaySecs > 0.0f;

    // (Re-)start the AudioComponent so the wave is actively pulling from
    // our queue when chunks land. Play() is idempotent.
    if (!bMuteAudio && AudioComponent)
    {
        AudioComponent->Play();
    }

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

    // Concatenate any prior odd-byte residue with this frame, then split
    // into an int16-aligned prefix + (optional) one-byte residue carried
    // to the next call. Without this, the orchestrator's tail chunk per
    // session (frequently odd-byte) trips the (Size % SampleByteSize)==0
    // ensure inside USoundWaveProcedural::QueueAudio.
    TArray<uint8> Aligned;
    Aligned.Reserve(AudioResidue.Num() + Size);
    Aligned.Append(AudioResidue);
    Aligned.Append(Data, Size);
    AudioResidue.Reset();

    const int32 EvenLen = Aligned.Num() & ~1;
    if (EvenLen == 0)
    {
        // Less than two bytes accumulated; keep all of it as residue.
        AudioResidue = MoveTemp(Aligned);
        return;
    }
    if (Aligned.Num() & 1)
    {
        AudioResidue.Add(Aligned[Aligned.Num() - 1]);
    }

    // Branch 1 — resample to 16 kHz and feed to A2F for blendshapes.
    const int32 NumSamples24k = EvenLen / 2;
    const int16* Samples24k = reinterpret_cast<const int16*>(Aligned.GetData());

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
        SessionStartBuffer.Append(Aligned.GetData(), EvenLen);
    }
    else
    {
        ProceduralWave->QueueAudio(Aligned.GetData(), EvenLen);
        UE_LOG(LogBoothFace, Verbose, TEXT("queued %d bytes; wave queue=%d"),
            EvenLen, ProceduralWave->GetAvailableAudioByteCount());
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
        const int32 N = SessionStartBuffer.Num();
        ProceduralWave->QueueAudio(SessionStartBuffer.GetData(), N);
        UE_LOG(LogBoothFace, Log, TEXT("preroll flush: %d bytes queued; wave queue=%d"),
            N, ProceduralWave->GetAvailableAudioByteCount());
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

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

#if WITH_ACE_RUNTIME
    // Translate the wire-format emotion map (lowercased A2F-3D emotion names
    // → 0..1 weights) into FAudio2FaceEmotion overrides. Returns unset when
    // the map is empty so SendAudioSamples falls through to A2E-only mode.
    TOptional<FAudio2FaceEmotion> BuildEmotion(
        const TMap<FString, float>& Emotions,
        float OverallStrength,
        float OverrideStrength)
    {
        if (Emotions.Num() == 0)
        {
            return TOptional<FAudio2FaceEmotion>{};
        }
        FAudio2FaceEmotion Out;
        Out.OverallEmotionStrength = FMath::Clamp(OverallStrength, 0.0f, 1.0f);
        Out.bEnableEmotionOverride = true;
        Out.EmotionOverrideStrength = FMath::Clamp(OverrideStrength, 0.0f, 1.0f);

        FAudio2FaceEmotionOverride& O = Out.EmotionOverrides;
        for (const auto& Pair : Emotions)
        {
            const float V = FMath::Clamp(Pair.Value, 0.0f, 1.0f);
            const FString Key = Pair.Key.ToLower();
            if      (Key == TEXT("amazement"))   { O.bOverrideAmazement = true;   O.Amazement = V; }
            else if (Key == TEXT("anger"))       { O.bOverrideAnger = true;       O.Anger = V; }
            else if (Key == TEXT("cheekiness"))  { O.bOverrideCheekiness = true;  O.Cheekiness = V; }
            else if (Key == TEXT("disgust"))     { O.bOverrideDisgust = true;     O.Disgust = V; }
            else if (Key == TEXT("fear"))        { O.bOverrideFear = true;        O.Fear = V; }
            else if (Key == TEXT("grief"))       { O.bOverrideGrief = true;       O.Grief = V; }
            else if (Key == TEXT("joy"))         { O.bOverrideJoy = true;         O.Joy = V; }
            else if (Key == TEXT("outofbreath")) { O.bOverrideOutOfBreath = true; O.OutOfBreath = V; }
            else if (Key == TEXT("pain"))        { O.bOverridePain = true;        O.Pain = V; }
            else if (Key == TEXT("sadness"))     { O.bOverrideSadness = true;     O.Sadness = V; }
        }
        return Out;
    }
#endif
}

ABoothFaceActor::ABoothFaceActor()
{
    PrimaryActorTick.bCanEverTick = false;
}

void ABoothFaceActor::BeginPlay()
{
    Super::BeginPlay();

#if WITH_ACE_RUNTIME
    // If a TargetCharacter is set in the editor, use ITS curve source
    // (the Apply ACE Face Animation anim node only finds curve sources
    // on the same actor it runs on, so this is required for MetaHuman
    // lipsync). Otherwise, fall back to a local curve source on this
    // actor — audio plays but no MetaHuman wiring.
    if (TargetCharacter)
    {
        AceCurveSource = TargetCharacter->FindComponentByClass<UACEAudioCurveSourceComponent>();
        if (AceCurveSource)
        {
            UE_LOG(LogBoothFace, Log, TEXT("ACE: using curve source on %s"), *TargetCharacter->GetName());
        }
        else
        {
            UE_LOG(LogBoothFace, Warning,
                TEXT("ACE: TargetCharacter %s has no UACEAudioCurveSourceComponent; falling back to local"),
                *TargetCharacter->GetName());
        }
    }
    if (!AceCurveSource)
    {
        AceCurveSource = NewObject<UACEAudioCurveSourceComponent>(this, TEXT("AceCurveSource"));
        SetRootComponent(AceCurveSource);
        AceCurveSource->RegisterComponent();
        UE_LOG(LogBoothFace, Log, TEXT("ACE: using local curve source (no TargetCharacter)"));
    }
    AceCurveSource->Volume = bMuteAudio ? 0.0f : 1.0f;
    AceCurveSource->BufferLengthInSeconds = AudioBufferSeconds;
    UE_LOG(LogBoothFace, Log, TEXT("ACE: audio pre-buffer set to %.2fs"), AudioBufferSeconds);

    // Hook playback lifecycle so we can attribute pipeline latency between
    // (A) ws/SendAudioSamples time and (B) A2F-3D NIM crunch time.
    AceCurveSource->OnAnimationStarted.AddDynamic(this, &ABoothFaceActor::HandleAnimationStarted);
    AceCurveSource->OnAnimationEnded.AddDynamic(this, &ABoothFaceActor::HandleAnimationEnded);

    // Configure the gRPC endpoint for the A2F-3D NIM.
    FACEConnectionInfo ConnectionInfo;
    ConnectionInfo.DestURL = A2FUrl;
    UACEBlueprintLibrary::SetA2XConnectionInfo(ConnectionInfo, A2FProviderName);

    // Pre-warm the gRPC connection so the first utterance's tts_end
    // doesn't beat the connection establishment. Without this, SID 0
    // closes its stream before any audio is sent and the NIM logs
    // "Audio2Face inference instance not provided".
    UACEBlueprintLibrary::AllocateA2F3DResources(A2FProviderName);

    // Silent pre-warm: 3 sessions × 1 s of silence, mirroring the runtime
    // pattern (CreateA2FStream → SendAudioSamples → EndOutgoingStream).
    // smoke_a2f shows the NIM itself has TTFB ~13 ms — yet the harness
    // measures ~5 s audio→anim gap on the first 2 real verdicts after a
    // fresh renderer launch, dropping to ~1.5 s by verdict 3+. A single
    // 100 ms pre-warm wasn't sufficient; the warming is cumulative. Three
    // 1-second silent sessions reproduce enough of the runtime lifecycle
    // to fully prime ACE/A2F state before any real verdict arrives.
    if (AceCurveSource)
    {
        const float SavedVolume = AceCurveSource->Volume;
        AceCurveSource->Volume = 0.0f;
        if (IA2FProvider* Provider = IA2FProvider::FindProvider(A2FProviderName))
        {
            const double WarmStart = FPlatformTime::Seconds();
            // 1 s of 16 kHz silence per session.
            TArray<int16> Silence;
            Silence.Init(0, AceSampleRate);
            constexpr int32 PreWarmSessions = 3;
            int32 SuccessfulSessions = 0;
            for (int32 i = 0; i < PreWarmSessions; ++i)
            {
                auto* WarmStream = Provider->CreateA2FStream(AceCurveSource);
                if (WarmStream)
                {
                    Provider->SendAudioSamples(
                        WarmStream,
                        TArrayView<const int16>(Silence.GetData(), Silence.Num()),
                        TOptional<FAudio2FaceEmotion>{},
                        nullptr);
                    Provider->EndOutgoingStream(WarmStream);
                    ++SuccessfulSessions;
                }
            }
            const double WarmDone = FPlatformTime::Seconds();
            UE_LOG(LogBoothFace, Log,
                TEXT("ACE: silent pre-warm done sessions=%d/%d total_ms=%.1f"),
                SuccessfulSessions, PreWarmSessions,
                (WarmDone - WarmStart) * 1000.0);
        }
        AceCurveSource->Volume = SavedVolume;
    }

    UE_LOG(LogBoothFace, Log, TEXT("ACE: configured provider %s -> %s (pre-warmed)"),
        *A2FProviderName.ToString(), *A2FUrl);
#endif

    WSClient = MakeUnique<FBoothWSClient>();
    WSClient->OnAudioSessionStart = [this](const FString& Fmt) { HandleAudioSessionStart(Fmt); };
    WSClient->OnAudioFrame        = [this](const uint8* D, int32 N) { HandleAudioFrame(D, N); };
    WSClient->OnAudioSessionEnd   = [this]() { HandleAudioSessionEnd(); };
    WSClient->OnTtsEmotion        = [this](const TMap<FString, float>& E, float O, float V) { HandleTtsEmotion(E, O, V); };
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
    SessionStartTime = FPlatformTime::Seconds();
    FirstFrameTime = 0.0;
    LastFrameTime = 0.0;
    SessionEndTime = 0.0;
    BytesThisSession = 0;
    FramesThisSession = 0;
    MaxFrameGapSec = 0.0;
    MaxSendCallSec = 0.0;
    TotalSendCallSec = 0.0;
    UE_LOG(LogBoothFace, Log, TEXT("audio session start: format=%s t=%.3f"), *Format, SessionStartTime);
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

    const double FrameArrivalTime = FPlatformTime::Seconds();
    const double FrameGapSec = (LastFrameTime > 0.0) ? (FrameArrivalTime - LastFrameTime) : 0.0;
    if (FirstFrameTime == 0.0) { FirstFrameTime = FrameArrivalTime; }
    if (FrameGapSec > MaxFrameGapSec) { MaxFrameGapSec = FrameGapSec; }
    LastFrameTime = FrameArrivalTime;
    BytesThisSession += Size;
    FramesThisSession += 1;

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
            TOptional<FAudio2FaceEmotion> Emotion = bHasCurrentEmotion
                ? BuildEmotion(CurrentEmotions, CurrentEmotionOverallStrength, CurrentEmotionOverrideStrength)
                : TOptional<FAudio2FaceEmotion>{};
            const double SendStart = FPlatformTime::Seconds();
            Provider->SendAudioSamples(
                Stream,
                TArrayView<const int16>(Samples16k.GetData(), Samples16k.Num()),
                Emotion,
                /*Audio2FaceParameters=*/ nullptr);
            // Pass the original 24 kHz audio through so ACE plays it
            // (no resampler artifacts) in sync with the blendshapes.
            if (IA2FPassthroughProvider* Passthrough = Provider->GetAudioPassthroughProvider())
            {
                Passthrough->EnqueueOriginalSamples(
                    Stream,
                    TArrayView<const uint8>(Aligned.GetData(), EvenLen));
            }
            const double SendDur = FPlatformTime::Seconds() - SendStart;
            TotalSendCallSec += SendDur;
            if (SendDur > MaxSendCallSec) { MaxSendCallSec = SendDur; }
            if (bLogPerFrameTiming)
            {
                UE_LOG(LogBoothFace, Log,
                    TEXT("frame #%d size=%d ws_gap_ms=%.1f send_ms=%.2f"),
                    FramesThisSession, Size, FrameGapSec * 1000.0, SendDur * 1000.0);
            }
        }
    }
#else
    (void)Samples16k;
#endif
}

void ABoothFaceActor::HandleAudioSessionEnd()
{
    SessionEndTime = FPlatformTime::Seconds();
    const double SessionDurSec = (SessionStartTime > 0.0) ? (SessionEndTime - SessionStartTime) : 0.0;
    const double FirstFrameLagSec = (FirstFrameTime > 0.0 && SessionStartTime > 0.0)
        ? (FirstFrameTime - SessionStartTime) : 0.0;
    const double InputAudioSec = (BytesThisSession > 0) ? (BytesThisSession / 48000.0) : 0.0;
    UE_LOG(LogBoothFace, Log,
        TEXT("audio session end: frames=%d bytes=%lld input_audio=%.2fs "
             "session_wall=%.2fs first_frame_lag_ms=%.1f "
             "max_ws_gap_ms=%.1f max_send_ms=%.2f total_send_ms=%.1f"),
        FramesThisSession, (long long)BytesThisSession, InputAudioSec,
        SessionDurSec, FirstFrameLagSec * 1000.0,
        MaxFrameGapSec * 1000.0, MaxSendCallSec * 1000.0, TotalSendCallSec * 1000.0);

    bHasCurrentEmotion = false;
    CurrentEmotions.Reset();

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

void ABoothFaceActor::HandleTtsEmotion(
    const TMap<FString, float>& Emotions,
    float OverallStrength,
    float OverrideStrength)
{
    CurrentEmotions = Emotions;
    CurrentEmotionOverallStrength = OverallStrength;
    CurrentEmotionOverrideStrength = OverrideStrength;
    bHasCurrentEmotion = (Emotions.Num() > 0);
    UE_LOG(LogBoothFace, Log,
        TEXT("tts_emotion: %d entries, overall=%.2f override=%.2f"),
        Emotions.Num(), OverallStrength, OverrideStrength);
}

void ABoothFaceActor::HandleAnimationStarted()
{
    const double Now = FPlatformTime::Seconds();
    // Two latencies that matter:
    //  - from_session_start: time since the orchestrator's tts_audio header
    //    landed = total wall time the user perceives as "press → first audio".
    //  - from_session_end: time spent waiting on A2F-3D *after* we finished
    //    sending audio = pure NIM crunch + curve source pre-buffer fill.
    const double FromStartMs = (SessionStartTime > 0.0) ? (Now - SessionStartTime) * 1000.0 : 0.0;
    const double FromEndMs   = (SessionEndTime   > 0.0) ? (Now - SessionEndTime)   * 1000.0 : 0.0;
    UE_LOG(LogBoothFace, Log,
        TEXT("animation_started: from_session_start_ms=%.1f from_session_end_ms=%.1f"),
        FromStartMs, FromEndMs);
}

void ABoothFaceActor::HandleAnimationEnded()
{
    const double Now = FPlatformTime::Seconds();
    const double FromStartMs = (SessionStartTime > 0.0) ? (Now - SessionStartTime) * 1000.0 : 0.0;
    UE_LOG(LogBoothFace, Log,
        TEXT("animation_ended: total_wall_ms=%.1f"), FromStartMs);
}

void ABoothFaceActor::HandleDisplayEvent(const FString& Type, const TSharedPtr<FJsonObject>& Event)
{
    UE_LOG(LogBoothFace, Verbose, TEXT("event: %s"), *Type);
}

void ABoothFaceActor::HandleConnectionChanged(bool bConnected)
{
    UE_LOG(LogBoothFace, Log, TEXT("ws %s"), bConnected ? TEXT("connected") : TEXT("disconnected"));
}

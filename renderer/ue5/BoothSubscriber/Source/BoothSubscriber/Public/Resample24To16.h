// Copyright (c) WetCourt. All rights reserved.

#pragma once

#include "CoreMinimal.h"

/**
 * Streaming polyphase resampler: 24000 Hz -> 16000 Hz, int16 mono.
 *
 * The orchestrator emits Kokoro TTS as PCM s16le at 24 kHz; the A2F-3D
 * NIM consumes 16 kHz mono. Rational ratio is L/M = 2/3.
 *
 * Implementation is a standard polyphase resampler with L=2 interpolation
 * factor and M=3 decimation factor, sharing one windowed-sinc prototype
 * (Hann-windowed, cutoff pi/3 on the upsampled grid) decomposed into L
 * sub-filters. State carries across `Process` calls so the caller can
 * stream arbitrary-sized chunks; ~8 output samples of warm-up transient
 * at session start.
 */
class BOOTHSUBSCRIBER_API FResample24To16
{
public:
    FResample24To16();

    /**
     * Consume `InNumSamples` int16 samples at 24 kHz; append resampled
     * output (16 kHz) to `OutSamples`. Returns the number of output
     * samples appended. Streaming-safe.
     */
    int32 Process(const int16* InSamples, int32 InNumSamples, TArray<int16>& OutSamples);

    /** Clear ring buffer + counters. Call between unrelated streams. */
    void Reset();

private:
    static constexpr int32 L = 2;             // upsample factor
    static constexpr int32 M = 3;             // decimation factor
    static constexpr int32 TapsPerPhase = 16; // length of each sub-filter
    static constexpr int32 ProtoLen = L * TapsPerPhase; // = 32

    // Polyphase decomposition: SubFilter[phase][m] = Proto[m*L + phase].
    float SubFilter[L][TapsPerPhase];

    // Ring buffer of most-recent input samples; size chosen to comfortably
    // hold one filter span plus a small slack.
    static constexpr int32 WindowLen = TapsPerPhase * 2;
    int16 Window[WindowLen];
    int32 WriteHead;       // next slot to write
    int64 InputAvailable;  // total input samples consumed since Reset
    int64 OutputCount;     // total output samples produced since Reset
};

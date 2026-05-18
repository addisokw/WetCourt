// Copyright (c) WetCourt. All rights reserved.

#include "Resample24To16.h"

#include "Math/UnrealMathUtility.h"

FResample24To16::FResample24To16()
{
    // Build the prototype FIR: windowed sinc, cutoff at 1/max(L,M) of the
    // upsampled Nyquist (= 1/3 normalized), Hann-windowed. The prototype
    // sits on the upsampled grid (L * input_rate); after decimation by M
    // the same filter serves as both the interpolation and anti-alias
    // lowpass.
    constexpr float Pi = 3.14159265358979323846f;
    constexpr float Cutoff = 1.0f / 3.0f; // normalized to upsampled Nyquist
    float Proto[ProtoLen];
    float Sum = 0.0f;
    const float Center = static_cast<float>(ProtoLen) * 0.5f - 0.5f;
    for (int32 i = 0; i < ProtoLen; ++i)
    {
        const float T = static_cast<float>(i) - Center;
        const float Arg = Pi * Cutoff * T;
        const float SincVal = (FMath::Abs(Arg) < 1e-6f) ? 1.0f : FMath::Sin(Arg) / Arg;
        const float Win = 0.5f - 0.5f * FMath::Cos(2.0f * Pi * (static_cast<float>(i) + 0.5f) / static_cast<float>(ProtoLen));
        Proto[i] = SincVal * Win;
        Sum += Proto[i];
    }
    // Normalize to unit DC gain, then multiply by L to compensate for
    // upsampling spectral attenuation. Sum is non-zero by construction.
    const float Scale = static_cast<float>(L) / Sum;
    for (int32 i = 0; i < ProtoLen; ++i)
    {
        Proto[i] *= Scale;
    }
    for (int32 Phase = 0; Phase < L; ++Phase)
    {
        for (int32 Tap = 0; Tap < TapsPerPhase; ++Tap)
        {
            SubFilter[Phase][Tap] = Proto[Tap * L + Phase];
        }
    }
    Reset();
}

void FResample24To16::Reset()
{
    FMemory::Memzero(Window, sizeof(Window));
    WriteHead = 0;
    InputAvailable = 0;
    OutputCount = 0;
}

int32 FResample24To16::Process(const int16* InSamples, int32 InNumSamples, TArray<int16>& OutSamples)
{
    const int32 InitialOut = OutSamples.Num();

    for (int32 i = 0; i < InNumSamples; ++i)
    {
        Window[WriteHead] = InSamples[i];
        WriteHead = (WriteHead + 1) % WindowLen;
        ++InputAvailable;

        // After this input, produce every output whose required latest
        // input sample is now available.
        while (true)
        {
            const int64 K = (OutputCount * M) / L;
            if (K >= InputAvailable)
            {
                break; // y[OutputCount] still needs future input
            }
            const int32 Phase = static_cast<int32>((OutputCount * M) % L);

            float Acc = 0.0f;
            for (int32 Tap = 0; Tap < TapsPerPhase; ++Tap)
            {
                const int64 InputIdx = K - Tap;
                if (InputIdx < 0)
                {
                    continue; // pre-stream zeros; warm-up transient
                }
                // RingIdx: walk back from WriteHead-1 (most recent) by
                // (InputAvailable-1 - InputIdx) slots, modulo WindowLen.
                const int64 Back = (InputAvailable - 1) - InputIdx;
                if (Back >= WindowLen)
                {
                    continue; // older than our window; treat as zero
                }
                int32 RingIdx = WriteHead - 1 - static_cast<int32>(Back);
                while (RingIdx < 0)
                {
                    RingIdx += WindowLen;
                }
                Acc += SubFilter[Phase][Tap] * static_cast<float>(Window[RingIdx]);
            }

            const int32 Clamped = FMath::Clamp(static_cast<int32>(Acc), -32768, 32767);
            OutSamples.Add(static_cast<int16>(Clamped));
            ++OutputCount;
        }
    }
    return OutSamples.Num() - InitialOut;
}

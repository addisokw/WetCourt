// Copyright (c) WetCourt. All rights reserved.

#pragma once

#include "CoreMinimal.h"
#include "GameFramework/Actor.h"
#include "Templates/SharedPointer.h"

#include "BoothOperatorActor.generated.h"

class FBoothInputPreProcessor;

/**
 * Standalone operator input → orchestrator HTTP relay. Drop one instance in
 * the level so the kiosk can trigger trials without alt-tabbing to the
 * browser operator panel.
 *
 * Default bindings:
 *   F1 → POST /operator/start
 *   F2 → POST /operator/plea  (skip charge dwell OR end plea early)
 *   F3 → POST /operator/estop
 *
 * Bindings dispatch through a Slate input pre-processor so PIE's editor
 * accelerators (F1-F8 = viewmode, Space/WASD/QE = fly cam, G = game view,
 * F8 = eject) can't swallow our keys. Override the FKey UPROPERTYs per-
 * instance in the editor if you want different keys.
 *
 * Console exec aliases (press `~` and type, in PIE or packaged):
 *   BoothStart
 *   BoothPlea
 *   BoothEStop
 */
UCLASS()
class BOOTHSUBSCRIBER_API ABoothOperatorActor : public AActor
{
    GENERATED_BODY()

public:
    ABoothOperatorActor();

    /** Base URL of the orchestrator's HTTP server (no trailing slash). */
    UPROPERTY(EditAnywhere, BlueprintReadOnly, Category = "Booth|Operator")
    FString OperatorBaseUrl = TEXT("http://127.0.0.1:8080");

    /** UE keyboard key that triggers a trial start. */
    UPROPERTY(EditAnywhere, BlueprintReadOnly, Category = "Booth|Operator")
    FKey StartKey = EKeys::F1;

    /** UE keyboard key that triggers the context-aware "Plea" action:
     *  cuts short the charge-display dwell while in DisplayingCharge, or
     *  ends plea capture early while in AwaitingPlea. */
    UPROPERTY(EditAnywhere, BlueprintReadOnly, Category = "Booth|Operator")
    FKey PleaKey = EKeys::F2;

    /** UE keyboard key that triggers an emergency stop. */
    UPROPERTY(EditAnywhere, BlueprintReadOnly, Category = "Booth|Operator")
    FKey EStopKey = EKeys::F3;

    /** Console-callable trial start (press `~`, type `BoothStart`). */
    UFUNCTION(Exec, BlueprintCallable, Category = "Booth|Operator")
    void BoothStart();

    /** Console-callable plea trigger (skip charge dwell / end plea early). */
    UFUNCTION(Exec, BlueprintCallable, Category = "Booth|Operator")
    void BoothPlea();

    /** Console-callable emergency stop. */
    UFUNCTION(Exec, BlueprintCallable, Category = "Booth|Operator")
    void BoothEStop();

protected:
    virtual void BeginPlay() override;
    virtual void EndPlay(const EEndPlayReason::Type EndPlayReason) override;
    virtual void SetupPlayerInputComponent(class UInputComponent* PlayerInputComponent);

private:
    void Post(const FString& Path, const TCHAR* OpLabel);
    void EnsureInputComponent();

    // Slate input pre-processor — receives key events ahead of the viewport
    // and editor accelerators, so PIE's F1-F8 viewmode shortcuts, fly-cam
    // (WASD/Space/Q/E), and Game View toggle (G) can't swallow our hotkeys.
    // Registered in BeginPlay, unregistered in EndPlay.
    TSharedPtr<FBoothInputPreProcessor> InputPreProcessor;
};

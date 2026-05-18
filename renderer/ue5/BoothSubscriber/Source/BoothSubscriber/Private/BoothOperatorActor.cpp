// Copyright (c) WetCourt. All rights reserved.

#include "BoothOperatorActor.h"

#include "Components/InputComponent.h"
#include "Engine/World.h"
#include "Framework/Application/IInputProcessor.h"
#include "Framework/Application/SlateApplication.h"
#include "GameFramework/PlayerController.h"
#include "HttpModule.h"
#include "Interfaces/IHttpRequest.h"
#include "Interfaces/IHttpResponse.h"
#include "InputCoreTypes.h"

DEFINE_LOG_CATEGORY_STATIC(LogBoothOperator, Log, All);

/**
 * Slate input pre-processor: receives key events at the Slate dispatch layer,
 * before they're routed to focused widgets, the viewport, or editor
 * accelerators. Returning true from HandleKeyDownEvent consumes the event so
 * nothing downstream (e.g. F1=viewmode-Lit, G=game-view) sees it.
 *
 * Holds a weak reference to the owning actor so a stale pre-processor
 * doesn't keep the actor alive past EndPlay.
 */
class FBoothInputPreProcessor : public IInputProcessor
{
public:
    explicit FBoothInputPreProcessor(ABoothOperatorActor* InOwner) : Owner(InOwner) {}

    virtual void Tick(const float, FSlateApplication&, TSharedRef<ICursor>) override {}

    virtual bool HandleKeyDownEvent(FSlateApplication&, const FKeyEvent& InKeyEvent) override
    {
        ABoothOperatorActor* OwnerPtr = Owner.Get();
        if (!OwnerPtr)
        {
            return false;
        }
        const FKey Key = InKeyEvent.GetKey();
        if (Key == OwnerPtr->StartKey) { OwnerPtr->BoothStart(); return true; }
        if (Key == OwnerPtr->PleaKey)  { OwnerPtr->BoothPlea();  return true; }
        if (Key == OwnerPtr->EStopKey) { OwnerPtr->BoothEStop(); return true; }
        return false;
    }

    virtual const TCHAR* GetDebugName() const override { return TEXT("BoothInputPreProcessor"); }

private:
    TWeakObjectPtr<ABoothOperatorActor> Owner;
};

ABoothOperatorActor::ABoothOperatorActor()
{
    PrimaryActorTick.bCanEverTick = false;
    // Receive input from the first local player so we can bind Space / Backspace
    // without forcing the user to wire a pawn or possession chain.
    AutoReceiveInput = EAutoReceiveInput::Player0;
}

void ABoothOperatorActor::BeginPlay()
{
    Super::BeginPlay();
    EnsureInputComponent();
    if (FSlateApplication::IsInitialized())
    {
        InputPreProcessor = MakeShared<FBoothInputPreProcessor>(this);
        FSlateApplication::Get().RegisterInputPreProcessor(InputPreProcessor);
    }
    UE_LOG(LogBoothOperator, Log,
        TEXT("operator ready: base=%s start=%s plea=%s estop=%s (slate pre-processor active)"),
        *OperatorBaseUrl, *StartKey.ToString(), *PleaKey.ToString(), *EStopKey.ToString());
}

void ABoothOperatorActor::EndPlay(const EEndPlayReason::Type EndPlayReason)
{
    if (InputPreProcessor.IsValid() && FSlateApplication::IsInitialized())
    {
        FSlateApplication::Get().UnregisterInputPreProcessor(InputPreProcessor);
    }
    InputPreProcessor.Reset();
    Super::EndPlay(EndPlayReason);
}

void ABoothOperatorActor::EnsureInputComponent()
{
    // AutoReceiveInput=Player0 creates an InputComponent for us in
    // EnableInput(); call it through the local player controller to make
    // sure the bindings actually receive key events.
    if (UWorld* World = GetWorld())
    {
        if (APlayerController* PC = World->GetFirstPlayerController())
        {
            EnableInput(PC);
        }
    }
    if (InputComponent)
    {
        SetupPlayerInputComponent(InputComponent);
    }
    else
    {
        UE_LOG(LogBoothOperator, Warning,
            TEXT("no InputComponent — operator hotkeys disabled (console BoothStart/BoothEStop still work)"));
    }
}

void ABoothOperatorActor::SetupPlayerInputComponent(UInputComponent* PlayerInputComponent)
{
    if (!PlayerInputComponent)
    {
        return;
    }
    PlayerInputComponent->BindKey(StartKey, IE_Pressed, this, &ABoothOperatorActor::BoothStart);
    PlayerInputComponent->BindKey(PleaKey,  IE_Pressed, this, &ABoothOperatorActor::BoothPlea);
    PlayerInputComponent->BindKey(EStopKey, IE_Pressed, this, &ABoothOperatorActor::BoothEStop);
}

void ABoothOperatorActor::BoothStart()
{
    Post(TEXT("/operator/start"), TEXT("start"));
}

void ABoothOperatorActor::BoothPlea()
{
    Post(TEXT("/operator/plea"), TEXT("plea"));
}

void ABoothOperatorActor::BoothEStop()
{
    Post(TEXT("/operator/estop"), TEXT("estop"));
}

void ABoothOperatorActor::Post(const FString& Path, const TCHAR* OpLabel)
{
    const FString Trimmed = OperatorBaseUrl.TrimChar(TEXT('/'));
    const FString Url = Trimmed + Path;
    UE_LOG(LogBoothOperator, Log, TEXT("operator %s -> %s"), OpLabel, *Url);

    TSharedRef<IHttpRequest> Req = FHttpModule::Get().CreateRequest();
    Req->SetVerb(TEXT("POST"));
    Req->SetURL(Url);
    // Orchestrator's /operator/start + /operator/estop don't read a body;
    // a Content-Length header is still polite for some axum versions.
    Req->SetHeader(TEXT("Content-Length"), TEXT("0"));
    const FString LabelCopy(OpLabel);
    Req->OnProcessRequestComplete().BindLambda(
        [LabelCopy](FHttpRequestPtr, FHttpResponsePtr Response, bool bSuccess)
        {
            if (bSuccess && Response.IsValid())
            {
                UE_LOG(LogBoothOperator, Log,
                    TEXT("operator %s -> HTTP %d"),
                    *LabelCopy, Response->GetResponseCode());
            }
            else
            {
                UE_LOG(LogBoothOperator, Warning,
                    TEXT("operator %s FAILED (orchestrator not reachable?)"), *LabelCopy);
            }
        });
    Req->ProcessRequest();
}

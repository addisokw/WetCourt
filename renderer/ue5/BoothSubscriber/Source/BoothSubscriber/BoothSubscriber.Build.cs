// Copyright (c) WetCourt. All rights reserved.

using UnrealBuildTool;

public class BoothSubscriber : ModuleRules
{
    public BoothSubscriber(ReadOnlyTargetRules Target) : base(Target)
    {
        PCHUsage = ModuleRules.PCHUsageMode.UseExplicitOrSharedPCHs;

        PublicDependencyModuleNames.AddRange(new string[]
        {
            "Core",
            "CoreUObject",
            "Engine",
        });

        PrivateDependencyModuleNames.AddRange(new string[]
        {
            "WebSockets",
            "Json",
            "JsonUtilities",
            "AudioMixer",
        });

        // NVIDIA ACE Unreal Plugin (NV_ACE_Reference v2.5.0-rc3 verified).
        // Real module names from its .uplugin — different from my draft
        // guesses. ACECore = IA2FProvider abstraction + types; ACERuntime
        // = UACEAudioCurveSourceComponent + Blueprint Library; A2FCommon
        // = shared parameter types.
        PublicDefinitions.Add("WITH_ACE_RUNTIME=1");
        PrivateDependencyModuleNames.AddRange(new string[]
        {
            "ACECore",
            "ACERuntime",
            "A2FCommon",
        });
    }
}

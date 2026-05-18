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

        // NVIDIA ACE Unreal Plugin module (added on the renderer PC once the
        // ACE plugin is installed; BoothFaceActor's ACE calls are gated by
        // WITH_ACE_RUNTIME and compile out when this is 0). Flip both lines
        // to 1 + add the module deps once ACE is installed.
        PublicDefinitions.Add("WITH_ACE_RUNTIME=0");
        // PrivateDependencyModuleNames.AddRange(new string[]
        // {
        //     "ACERuntime",
        //     "ACEAudio2Face",
        // });
    }
}

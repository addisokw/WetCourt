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
        // ACE plugin is installed; the BoothFaceActor's ACE calls compile
        // out via #if BOOTH_ACE_AVAILABLE until then). Uncomment when ready:
        //
        // PrivateDependencyModuleNames.AddRange(new string[]
        // {
        //     "ACERuntime",
        //     "ACEAudio2Face",
        // });
    }
}

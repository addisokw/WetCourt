# Avatars

`judge.glb` is the model loaded by `src/face.ts`. The committed placeholder
is TalkingHead's `brunette.glb` reference avatar (Ready Player Me, ARKit-52
+ Oculus visemes, ~4.7 MB).

To replace with a custom-baked judge:

1. Sign in to [readyplayer.me](https://readyplayer.me).
2. Build the avatar in their wizard. White wig isn't in their wardrobe; pick
   the closest match or upload a photo for likeness.
3. Copy the avatar URL and append `?morphTargets=ARKit,Oculus+Visemes&textureAtlas=1024`
   to bake in the lipsync rig and reduce texture count.
4. Download the `.glb` and overwrite `judge.glb` in this directory.
5. Verify in Blender or the three.js editor that the mesh has the ARKit-52
   morph targets (`jawOpen`, `mouthFunnel`, `mouthSmileLeft`, etc.) — the
   face module skips morph writes silently if the mesh doesn't have them.

If you generate a custom artist-made head, follow TalkingHead's
[avatar prep guide](https://github.com/met4citizen/TalkingHead/blob/main/READY.md)
for the bone-name + morph-target conventions.

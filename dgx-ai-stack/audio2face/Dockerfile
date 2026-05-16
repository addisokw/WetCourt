# syntax=docker/dockerfile:1.7
# Audio2Face-3D on arm64 + GB10 (sm_121).
#
# Strategy (in increasing order of effort):
#
#   1. NIM container — `nvcr.io/nim/nvidia/audio2face-3d`. Not built here;
#      if it works on aarch64 you swap the image in docker-compose.yml directly
#      and skip this Dockerfile. NIM compatibility on sm_121 is unverified and
#      other NIM containers have been known to fail with cudaErrorSymbolNotFound
#      on this hardware, so this Dockerfile exists as the fallback.
#
#   2. PyPI `nvidia-audio2face-3d` — open-sourced 2025. May or may not ship
#      aarch64 wheels. server.py imports lazily and falls back to an
#      amplitude-driven synthetic stub if the import fails, so the container
#      still serves and the plumbing is smoke-testable.
#
#   3. C++/CUDA + TensorRT source build of NVIDIA/Audio2Face-3D-SDK for sm_121,
#      mirroring the whisper-cpp pattern (build inside the image with -DSM=121).
#      Out of scope for the spike's first iteration; revisit if (1) and (2) both
#      fail and the feasibility answer is still wanted.
#
# Base image matches parakeet's (NGC PyTorch 25.11 for arm64+Blackwell).
FROM nvcr.io/nvidia/pytorch:25.11-py3

ENV DEBIAN_FRONTEND=noninteractive \
    NVIDIA_VISIBLE_DEVICES=all \
    NVIDIA_DRIVER_CAPABILITIES=compute,utility \
    HF_HUB_DOWNLOAD_TIMEOUT=600 \
    HF_HOME=/models/hf-cache \
    PYTHONUNBUFFERED=1

RUN apt-get update && apt-get install -y --no-install-recommends \
    ffmpeg libsndfile1 \
 && rm -rf /var/lib/apt/lists/*

# nvidia-audio2face-3d on PyPI is a *gRPC client* (protobuf stubs only, no model
# code, no native dependencies). It's useful as the client surface to talk to a
# real A2F-3D backend, but installing it does NOT give us local inference. To
# actually run A2F-3D on this hardware we still need either the NIM container
# or a from-source SDK build. server.py treats this package's absence as the
# signal to drop to the amplitude-stub plumbing mode.
RUN pip install --no-cache-dir \
        fastapi "uvicorn[standard]" websockets numpy soundfile grpcio protobuf \
 && pip install --no-cache-dir "nvidia-audio2face-3d" || \
    echo "WARN: nvidia-audio2face-3d (client stubs) install failed; running amplitude-stub only"

WORKDIR /app
COPY server.py /app/server.py

EXPOSE 9000
CMD ["python", "/app/server.py"]

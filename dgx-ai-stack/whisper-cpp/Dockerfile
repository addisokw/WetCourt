# syntax=docker/dockerfile:1.7
ARG CUDA_TAG=13.0.0
ARG UBUNTU=ubuntu24.04
ARG CUDA_ARCH=121

FROM --platform=linux/arm64 nvcr.io/nvidia/cuda:${CUDA_TAG}-devel-${UBUNTU} AS build
ARG CUDA_ARCH
ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update && apt-get install -y --no-install-recommends \
    git ca-certificates cmake build-essential pkg-config \
 && rm -rf /var/lib/apt/lists/*

# nvcr cuda devel images ship libcuda.so only as a stub (the real lib is
# injected at runtime by the nvidia container toolkit). Point the linker
# at the stubs dir so -lcuda resolves at build time.
ENV LIBRARY_PATH=/usr/local/cuda/lib64/stubs:${LIBRARY_PATH}

WORKDIR /src
RUN git clone --depth=1 https://github.com/ggml-org/whisper.cpp.git
WORKDIR /src/whisper.cpp
RUN cmake -B build \
      -DCMAKE_BUILD_TYPE=Release \
      -DGGML_CUDA=1 \
      -DGGML_CUDA_NO_VMM=ON \
      -DCMAKE_CUDA_ARCHITECTURES=${CUDA_ARCH} \
      -DWHISPER_BUILD_TESTS=OFF \
      -DWHISPER_BUILD_EXAMPLES=ON \
      -DWHISPER_SDL2=OFF \
 && cmake --build build -j --target whisper-server

# Gather every shared library produced anywhere in the build tree so the
# runtime stage can grab them with one COPY.
RUN mkdir -p /libs && find build -name "*.so*" -exec cp -P {} /libs/ \;

FROM --platform=linux/arm64 nvcr.io/nvidia/cuda:${CUDA_TAG}-runtime-${UBUNTU} AS runtime
ENV DEBIAN_FRONTEND=noninteractive \
    NVIDIA_VISIBLE_DEVICES=all \
    NVIDIA_DRIVER_CAPABILITIES=compute,utility
RUN apt-get update && apt-get install -y --no-install-recommends \
    libgomp1 ca-certificates \
 && rm -rf /var/lib/apt/lists/*

WORKDIR /app
COPY --from=build /src/whisper.cpp/build/bin/whisper-server /app/whisper-server
COPY --from=build /libs/ /usr/local/lib/
RUN ldconfig

EXPOSE 8081
ENTRYPOINT ["/app/whisper-server"]
CMD ["--host", "0.0.0.0", "--port", "8081", \
     "--inference-path", "/v1/audio/transcriptions", \
     "-m", "/models/ggml-large-v3-turbo.bin", \
     "-l", "auto", "-t", "8"]

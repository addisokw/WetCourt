# syntax=docker/dockerfile:1.7
# LEGACY (2026-06-10): the stack moved to Qwen3.6-35B-A3B-NVFP4 on vLLM (see ../README.md).
# This image is no longer built or referenced by docker-compose.yml — kept only as a
# llama.cpp fallback. Do not rebuild unless you are reverting to the GGUF path.
# Packages the pre-built llama-server binary + libs from
# ~/ken-test/opensauce/llama.cpp/build/bin (HEAD has qwen35moe support).
# Build context must be that build/bin directory.
ARG CUDA_TAG=13.0.0
ARG UBUNTU=ubuntu24.04

FROM --platform=linux/arm64 nvcr.io/nvidia/cuda:${CUDA_TAG}-runtime-${UBUNTU}
ENV DEBIAN_FRONTEND=noninteractive \
    NVIDIA_VISIBLE_DEVICES=all \
    NVIDIA_DRIVER_CAPABILITIES=compute,utility \
    LD_LIBRARY_PATH=/app:/usr/local/cuda/lib64
RUN apt-get update && apt-get install -y --no-install-recommends \
    libgomp1 ca-certificates curl \
 && rm -rf /var/lib/apt/lists/*

WORKDIR /app
# llama-server keeps libs alongside the binary; LD_LIBRARY_PATH covers it.
COPY llama-server /app/llama-server
COPY libllama.so* libllama-common.so* libllama-server-impl.so* libmtmd.so* libggml*.so* /app/
RUN chmod +x /app/llama-server

EXPOSE 8000
ENTRYPOINT ["/app/llama-server"]
CMD ["--host", "0.0.0.0", "--port", "8000"]

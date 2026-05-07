# syntax=docker/dockerfile:1.7
# Parakeet TDT 0.6B v2 on arm64 + Blackwell. Built on top of the NGC PyTorch
# image already present on the box, which has the right torch/CUDA stack.
FROM nvcr.io/nvidia/pytorch:25.11-py3

ENV DEBIAN_FRONTEND=noninteractive \
    NVIDIA_VISIBLE_DEVICES=all \
    NVIDIA_DRIVER_CAPABILITIES=compute,utility \
    HF_HUB_DOWNLOAD_TIMEOUT=600 \
    HF_HOME=/models/hf-cache

RUN apt-get update && apt-get install -y --no-install-recommends \
    ffmpeg libsndfile1 \
 && rm -rf /var/lib/apt/lists/*

# NeMo's ASR collection pulls in a large dep tree. numpy<2 avoids the known
# incompatibility (NeMo issue #14505).
RUN pip install --no-cache-dir \
        "numpy<2" \
        "nemo_toolkit[asr]>=2.2.0" \
        soundfile scipy \
        fastapi uvicorn[standard] python-multipart

WORKDIR /app
COPY server.py /app/server.py

EXPOSE 8082
CMD ["python", "/app/server.py"]

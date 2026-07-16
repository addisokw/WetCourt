# Turret vision for the DGX Spark (aarch64) — built from ../vision by
# dgx-ai-stack/docker-compose.yml. CPU-only: MediaPipe ships CPU wheels for
# linux aarch64, no CUDA needed (see pyproject.toml).
FROM python:3.11-slim-bookworm

# opencv-python's runtime shared libs (the wheel bundles ffmpeg/v4l2 itself).
RUN apt-get update \
    && apt-get install -y --no-install-recommends libgl1 libglib2.0-0 \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

# Same deps as pyproject.toml, but mediapipe is pinned: 0.10.18 is the LAST
# release with linux aarch64 wheels (cp310-cp312) — anything newer has no
# Spark-compatible wheel and the install fails.
RUN pip install --no-cache-dir \
    "mediapipe==0.10.18" \
    "opencv-python>=4.8" \
    "flask>=3.0" \
    "numpy>=1.24" \
    "requests>=2.31"

COPY vision.py ./

# models/ is bind-mounted with the pre-downloaded pose_landmarker_lite.task so
# the booth needs no internet at the venue (vision.py would otherwise fetch it
# on first run). Camera, port, gains etc. come via BOOTH_VISION_* env.
CMD ["python", "vision.py"]

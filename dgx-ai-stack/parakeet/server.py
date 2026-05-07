"""OpenAI-shaped /v1/audio/transcriptions wrapper for nvidia/parakeet-tdt-0.6b-v2.

Loads the NeMo ASR model once at startup, then services multipart uploads.
"""
import io
import os
import tempfile
import wave

import nemo.collections.asr as nemo_asr
import soundfile as sf
import uvicorn
from fastapi import FastAPI, File, Form, HTTPException, UploadFile
from fastapi.responses import JSONResponse, PlainTextResponse

MODEL_NAME = os.environ.get("PARAKEET_MODEL", "nvidia/parakeet-tdt-0.6b-v2")

app = FastAPI()
print(f"Loading {MODEL_NAME} ...", flush=True)
asr_model = nemo_asr.models.ASRModel.from_pretrained(model_name=MODEL_NAME)
asr_model.eval()
print("Parakeet ready.", flush=True)


@app.get("/health")
def health():
    return {"status": "ok", "model": MODEL_NAME}


@app.post("/v1/audio/transcriptions")
async def transcribe(
    file: UploadFile = File(...),
    model: str | None = Form(None),
    language: str | None = Form(None),
    prompt: str | None = Form(None),
    response_format: str = Form("json"),
    temperature: float = Form(0.0),
):
    raw = await file.read()
    if not raw:
        raise HTTPException(400, "empty audio")

    # Write to a temp file. Parakeet needs a path; soundfile resamples to 16k.
    with tempfile.NamedTemporaryFile(suffix=".wav", delete=False) as tmp:
        path = tmp.name
    try:
        try:
            data, sr = sf.read(io.BytesIO(raw), always_2d=False)
        except Exception:
            with open(path, "wb") as f:
                f.write(raw)
        else:
            if data.ndim > 1:
                data = data.mean(axis=1)
            if sr != 16000:
                import numpy as np
                from scipy import signal
                target = 16000
                n = round(len(data) * target / sr)
                data = signal.resample(data, n).astype("float32")
                sr = target
            sf.write(path, data, sr, subtype="PCM_16")

        result = asr_model.transcribe([path])
        text = result[0].text if hasattr(result[0], "text") else str(result[0])
    finally:
        try:
            os.unlink(path)
        except FileNotFoundError:
            pass

    if response_format == "text":
        return PlainTextResponse(text)
    return JSONResponse({"text": text})


if __name__ == "__main__":
    uvicorn.run(app, host="0.0.0.0", port=8082)

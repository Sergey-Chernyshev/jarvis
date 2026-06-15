#!/usr/bin/env python3
"""Silero TTS сайдкар Jarvis. Только localhost. Текст → WAV, модель в памяти.

Запуск (демон делает сам через venv-python):
    python silero-server.py --port N --speaker baya --model v4_ru

Контракт:
    GET  /health           → {"ok": true, "model": "v4_ru"}
    POST /tts {text, speaker?, sample_rate?} → audio/wav (16-bit PCM, моно)

Никакой сети наружу: слушаем только 127.0.0.1. Модель грузится один раз на старте.
"""
import argparse
import io
import wave

import numpy as np
import torch
import uvicorn
from fastapi import FastAPI, Response
from pydantic import BaseModel

ap = argparse.ArgumentParser()
ap.add_argument("--port", type=int, required=True)
ap.add_argument("--model", default="v4_ru")  # v4_ru | v5_ru — свериться на живой системе
ap.add_argument("--speaker", default="baya")
args = ap.parse_args()

# CPU-инференс, пара потоков — реплики короткие, греть все ядра незачем
torch.set_num_threads(2)
device = torch.device("cpu")
model, _ = torch.hub.load(
    "snakers4/silero-models", "silero_tts", language="ru", speaker=args.model
)
model.to(device)
DEFAULT_SPEAKER = args.speaker

app = FastAPI()


class Req(BaseModel):
    text: str
    speaker: str | None = None
    sample_rate: int = 24000


@app.get("/health")
def health():
    return {"ok": True, "model": args.model}


@app.post("/tts")
def tts(r: Req):
    text = (r.text or "").strip() or "."
    speaker = r.speaker or DEFAULT_SPEAKER
    sr = r.sample_rate if r.sample_rate in (8000, 24000, 48000) else 24000
    audio = model.apply_tts(text=text, speaker=speaker, sample_rate=sr)
    pcm = (np.clip(audio.numpy(), -1.0, 1.0) * 32767).astype("<i2").tobytes()
    buf = io.BytesIO()
    with wave.open(buf, "wb") as w:
        w.setnchannels(1)
        w.setsampwidth(2)
        w.setframerate(sr)
        w.writeframes(pcm)
    return Response(content=buf.getvalue(), media_type="audio/wav")


if __name__ == "__main__":
    uvicorn.run(app, host="127.0.0.1", port=args.port, log_level="warning")

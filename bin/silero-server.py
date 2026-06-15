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
import os
import wave

# python.org Python без системных CA → torch.hub по HTTPS падает на верификации.
# Берём CA-бандл из certifi ДО импорта torch. Делает сайдкар самодостаточным,
# как бы его ни спавнил демон.
try:
    import certifi
    os.environ.setdefault("SSL_CERT_FILE", certifi.where())
    os.environ.setdefault("REQUESTS_CA_BUNDLE", certifi.where())
except Exception:
    pass

import numpy as np
import torch
import uvicorn
from fastapi import FastAPI, Response
from pydantic import BaseModel

ap = argparse.ArgumentParser()
ap.add_argument("--port", type=int, required=True)
ap.add_argument("--model", default="v4_ru")  # v4_ru | v5_ru — свериться на живой системе
ap.add_argument("--speaker", default="baya")
ap.add_argument("--rate", default="fast")  # x-slow|slow|medium|fast|x-fast
args = ap.parse_args()

import html

# допустимые значения скорости (SSML <prosody rate>) — только ключевые слова
VALID_RATE = {"x-slow", "slow", "medium", "fast", "x-fast"}

# CPU-инференс, пара потоков — реплики короткие, греть все ядра незачем
torch.set_num_threads(2)
device = torch.device("cpu")
model, _ = torch.hub.load(
    "snakers4/silero-models", "silero_tts", language="ru", speaker=args.model,
    trust_repo=True,  # модель уже в кэше; не спрашивать про доверие к репо
)
model.to(device)
DEFAULT_SPEAKER = args.speaker
DEFAULT_RATE = args.rate if args.rate in VALID_RATE else "fast"

app = FastAPI()


class Req(BaseModel):
    text: str
    speaker: str | None = None
    sample_rate: int = 48000
    rate: str | None = None  # x-slow|slow|medium|fast|x-fast


@app.get("/health")
def health():
    return {"ok": True, "model": args.model, "rate": DEFAULT_RATE}


@app.post("/tts")
def tts(r: Req):
    text = (r.text or "").strip() or "."
    speaker = r.speaker or DEFAULT_SPEAKER
    # 48 кГц по умолчанию — лучшее качество модели
    sr = r.sample_rate if r.sample_rate in (8000, 24000, 48000) else 48000
    rate = r.rate if (r.rate in VALID_RATE) else DEFAULT_RATE
    # SSML управляет темпом речи; текст экранируем (& < > сломали бы XML).
    # put_accent/put_yo=True — корректные ударения и «ё». Если SSML подавился —
    # фолбэк на обычный текст, чтобы реплика не пропала.
    safe = html.escape(text, quote=False)
    ssml = f'<speak><prosody rate="{rate}">{safe}</prosody></speak>'
    try:
        audio = model.apply_tts(
            ssml_text=ssml, speaker=speaker, sample_rate=sr, put_accent=True, put_yo=True
        )
    except Exception:
        audio = model.apply_tts(
            text=text, speaker=speaker, sample_rate=sr, put_accent=True, put_yo=True
        )
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

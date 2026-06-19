#!/usr/bin/env python3
"""Qwen3-ASR STT сайдкар Jarvis. Только localhost. PCM → текст, модель в памяти.

Запуск (демон делает сам через venv-python):
    python stt-server.py --port N --model qwen3-0.6b

Контракт:
    GET  /health           → {"ok": true, "model": "qwen3-0.6b"}
    POST /transcribe       тело = little-endian float32 PCM 16кГц моно
                         → {"text": "...", "segments": [...]}

Никакой сети наружу: слушаем только 127.0.0.1. Модель грузится один раз на старте.
"""
import argparse
import os

# python.org Python без системных CA → HuggingFace Hub по HTTPS падает на верификации.
# Берём CA-бандл из certifi ДО импорта модельных библиотек. Делает сайдкар
# самодостаточным, как бы его ни спавнил демон.
try:
    import certifi
    os.environ.setdefault("SSL_CERT_FILE", certifi.where())
    os.environ.setdefault("REQUESTS_CA_BUNDLE", certifi.where())
except Exception:
    pass

import numpy as np
import uvicorn
from typing import Optional
from fastapi import FastAPI, Request, Response
from fastapi.responses import JSONResponse

ap = argparse.ArgumentParser()
ap.add_argument("--port", type=int, required=True)
ap.add_argument("--model", default="qwen3-0.6b")  # qwen3-0.6b | qwen3-1.7b
args = ap.parse_args()

# Таблица: имя модели → HuggingFace репо
_MODEL_REPOS = {
    "qwen3-0.6b": "mlx-community/Qwen3-ASR-0.6B-8bit",
    "qwen3-1.7b": "mlx-community/Qwen3-ASR-1.7B-4bit",
}

model_repo = _MODEL_REPOS.get(args.model, _MODEL_REPOS["qwen3-0.6b"])

# TODO: сверить на живой установке — API qwen3_asr_mlx не верифицирован без live-установки
from qwen3_asr_mlx import Qwen3ASR  # type: ignore[import]
model = Qwen3ASR.from_pretrained(model_repo)

app = FastAPI()


@app.get("/health")
def health():
    return {"ok": True, "model": args.model}


@app.post("/transcribe")
async def transcribe(request: Request):
    try:
        body = await request.body()
        # Парсить тело как little-endian float32 PCM 16кГц моно
        pcm = np.frombuffer(body, dtype="<f4")
        # TODO: сверить на живой установке — API qwen3_asr_mlx не верифицирован без live-установки
        result = model.transcribe(pcm)
        resp = {"text": result.text}
        if hasattr(result, "segments"):
            resp["segments"] = result.segments
        return JSONResponse(content=resp)
    except Exception as exc:
        return JSONResponse(status_code=500, content={"error": str(exc)})


if __name__ == "__main__":
    uvicorn.run(app, host="127.0.0.1", port=args.port, log_level="warning")

#!/usr/bin/env python3
"""Silero TTS сайдкар Jarvis. Только localhost. Текст → WAV, модель в памяти.

Запуск (демон делает сам через venv-python):
    python silero-server.py --port N --speaker xenia --model v4_ru

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
from typing import Optional
from fastapi import FastAPI, Response
from pydantic import BaseModel

ap = argparse.ArgumentParser()
ap.add_argument("--port", type=int, required=True)
ap.add_argument("--model", default="v4_ru")  # v4_ru | v5_ru — свериться на живой системе
ap.add_argument("--speaker", default="xenia")
ap.add_argument("--rate", default="fast")  # x-slow|slow|medium|fast|x-fast
args = ap.parse_args()

import html
import re

# допустимые значения скорости (SSML <prosody rate>) — только ключевые слова
VALID_RATE = {"x-slow", "slow", "medium", "fast", "x-fast"}

# --- нормализация под русскую модель: латиницы и цифр в алфавите v4_ru НЕТ,
# модель их просто глотает. Транслитерируем латиницу в кириллицу (как и
# произносят: «гитлаб», «докер»), числа разворачиваем в слова. ---

# частые тех-термины → русское звучание (приоритетнее побуквенной транслитерации)
_TERMS = {
    "gitlab": "гитлаб", "github": "гитхаб", "git": "гит", "docker": "докер",
    "compose": "компоуз", "docker-compose": "докер компоуз", "bash": "баш",
    "zsh": "зэ-эс-эйч", "pr": "пи-ар", "api": "апи", "ci": "си-ай", "cd": "си-ди",
    "json": "джейсон", "yaml": "ямл", "npm": "эн-пи-эм", "readme": "ридми",
    "commit": "коммит", "push": "пуш", "pull": "пул", "merge": "мёрж",
    "branch": "бранч", "build": "билд", "test": "тест", "tests": "тесты",
    "rust": "раст", "cargo": "карго", "python": "пайтон", "node": "ноуд",
    "claude": "клод", "ok": "окей", "url": "ю-эр-эл", "http": "эйч-ти-ти-пи",
    "https": "эйч-ти-ти-пи-эс", "sql": "эс-кью-эл", "ssh": "эс-эс-эйч",
    "env": "энв", "todo": "туду", "ui": "ю-ай", "id": "ай-ди", "ide": "ай-ди-и",
}
_DIGRAPHS = [("sch", "ш"), ("sh", "ш"), ("ch", "ч"), ("ph", "ф"), ("th", "т"),
             ("ck", "к"), ("zh", "ж"), ("kh", "х"), ("yo", "ё"), ("ya", "я"),
             ("yu", "ю"), ("ts", "ц"), ("qu", "кв"), ("ee", "и"), ("oo", "у")]
_LETTERS = {"a": "а", "b": "б", "c": "к", "d": "д", "e": "е", "f": "ф", "g": "г",
            "h": "х", "i": "и", "j": "дж", "k": "к", "l": "л", "m": "м", "n": "н",
            "o": "о", "p": "п", "q": "к", "r": "р", "s": "с", "t": "т", "u": "у",
            "v": "в", "w": "в", "x": "кс", "y": "й", "z": "з"}

_ONES = ["ноль", "один", "два", "три", "четыре", "пять", "шесть", "семь", "восемь", "девять"]
_TEENS = ["десять", "одиннадцать", "двенадцать", "тринадцать", "четырнадцать",
          "пятнадцать", "шестнадцать", "семнадцать", "восемнадцать", "девятнадцать"]
_TENS = ["", "", "двадцать", "тридцать", "сорок", "пятьдесят", "шестьдесят",
         "семьдесят", "восемьдесят", "девяносто"]
_HUND = ["", "сто", "двести", "триста", "четыреста", "пятьсот", "шестьсот",
         "семьсот", "восемьсот", "девятьсот"]


def _u1000(n):
    p = []
    if n >= 100:
        p.append(_HUND[n // 100]); n %= 100
    if 10 <= n <= 19:
        p.append(_TEENS[n - 10]); n = 0
    elif n >= 20:
        p.append(_TENS[n // 10]); n %= 10
    if 0 < n < 10:
        p.append(_ONES[n])
    return " ".join(p)


def _num2words(n):
    n = int(n)
    if n == 0:
        return "ноль"
    parts = []
    th, r = n // 1000, n % 1000
    if th:
        w = _u1000(th).replace("один", "одна").replace("два", "две")
        if th % 10 == 1 and th % 100 != 11:
            f = "тысяча"
        elif 2 <= th % 10 <= 4 and not 12 <= th % 100 <= 14:
            f = "тысячи"
        else:
            f = "тысяч"
        parts.append(f"{w} {f}")
    if r:
        parts.append(_u1000(r))
    return " ".join(parts)


def _translit_word(w):
    lw = w.lower()
    if lw in _TERMS:
        return _TERMS[lw]
    s = lw
    for a, b in _DIGRAPHS:
        s = s.replace(a, b)
    return "".join(_LETTERS.get(c, c) for c in s)


def normalize(text):
    """Латиница → кириллица, числа → слова. Иначе модель их глотает."""
    def _lat(m):
        tok = m.group(0)
        if tok.lower() in _TERMS:
            return _TERMS[tok.lower()]
        return " ".join(_translit_word(p) for p in re.split(r"[-_./]", tok) if p)
    text = re.sub(r"[A-Za-z][A-Za-z0-9\-_./]*", _lat, text)
    text = re.sub(r"\d+", lambda m: _num2words(m.group(0)), text)
    return text

# CPU-инференс, пара потоков — реплики короткие, греть все ядра незачем
torch.set_num_threads(2)
device = torch.device("cpu")
model, _ = torch.hub.load(
    "snakers4/silero-models", "silero_tts", language="ru", speaker=args.model,
    trust_repo=True,  # модель уже в кэше; не спрашивать про доверие к репо
    skip_validation=True,  # не ходить в GitHub API: там легко поймать rate limit
)
model.to(device)
DEFAULT_SPEAKER = args.speaker
DEFAULT_RATE = args.rate if args.rate in VALID_RATE else "fast"

app = FastAPI()


class Req(BaseModel):
    text: str
    speaker: Optional[str] = None  # PEP 604 «str | None» не работает на Python 3.9 (системный python3 macOS)
    sample_rate: int = 48000
    rate: Optional[str] = None  # x-slow|slow|medium|fast|x-fast


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
    # сейфти-нет: латиница→кириллица, числа→слова (модель их иначе глотает).
    # Если выше дали уже русский `speak` — тут ничего лишнего не случится.
    text = normalize(text)
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

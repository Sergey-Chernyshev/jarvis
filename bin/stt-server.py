#!/usr/bin/env python3
"""Qwen3-ASR STT сайдкар Jarvis. Только localhost. PCM → текст, модель в памяти.

Запуск (демон делает сам через venv-python):
    python stt-server.py --port N --model qwen3-0.6b [--lang ru]

Контракт:
    GET  /health      → {"ok": true, "model": "qwen3-0.6b", "source": ...}
    POST /transcribe  тело = little-endian float32 PCM 16кГц моно;
                      язык — заголовок `lang` (иначе дефолт --lang)
                    → {"text": "...", "segments": [...]}

Локально: слушаем только 127.0.0.1. Модель грузится один раз на старте.

Веса: если рядом со скриптом есть папка `models/<имя>/config.json` — грузим
ОТТУДА (для сетей, где huggingface.co заблокирован: скачай веса где угодно и
положи в эту папку); иначе тянем mlx-community-репо с HF (HF_ENDPOINT уважается).
"""
import argparse
import os

# CA-бандл из certifi до импорта модельных библиотек (как в silero-сайдкаре).
try:
    import certifi
    os.environ.setdefault("SSL_CERT_FILE", certifi.where())
    os.environ.setdefault("REQUESTS_CA_BUNDLE", certifi.where())
except Exception:
    pass

import numpy as np
import uvicorn
from typing import Optional  # noqa: F401  (Python 3.9 совместимость — без PEP604)
from fastapi import FastAPI, Request
from fastapi.responses import JSONResponse

ap = argparse.ArgumentParser()
ap.add_argument("--port", type=int, required=True)
ap.add_argument("--model", default="qwen3-0.6b")  # qwen3-0.6b | qwen3-1.7b
ap.add_argument("--lang", default="ru")  # доминирующий язык (пин); транскрипция, не перевод
args = ap.parse_args()

_MODEL_REPOS = {
    "qwen3-0.6b": "mlx-community/Qwen3-ASR-0.6B-8bit",
    "qwen3-1.7b": "mlx-community/Qwen3-ASR-1.7B-4bit",
}

# Локальная папка весов рядом со скриптом — приоритетнее HF (для заблок. сетей).
_here = os.path.dirname(os.path.abspath(__file__))
_local = os.path.join(_here, "models", args.model)
if os.path.isfile(os.path.join(_local, "config.json")):
    model_src = _local
else:
    model_src = _MODEL_REPOS.get(args.model, _MODEL_REPOS["qwen3-0.6b"])

# API сверен живой установкой qwen3-asr-mlx 0.1.0:
#   Qwen3ASR.from_pretrained(repo|local_path) -> model
#   model.transcribe(audio: np.ndarray|str|Path, language="ru") -> TranscriptionResult(.text)
from qwen3_asr_mlx import Qwen3ASR  # type: ignore[import]

# --- Поддержка КВАНТОВАННЫХ (MLX) весов текстового декодера --------------------
# qwen3-asr-mlx 0.1.1 строит текстовый декодер из ПЛОТНЫХ nn.Linear и грузит веса
# без вызова nn.quantize (model.py:from_pretrained → decoder.py:load_decoder_weights).
# Для 8bit/4bit репозиториев mlx-community веса model.layers.* лежат как
# QuantizedLinear (.weight+.scales+.biases) → load_weights падает:
#   "Received N parameters not in model: ...scales, ...biases" (decoder.py:290).
# Здесь оборачиваем штатный загрузчик декодера: ДО load_weights квантуем ровно те
# подмодули, у которых в файле есть .scales, параметрами из config["quantization"]
# (group_size/bits/mode). Энкодер (audio_tower) в этих репо НЕ квантован (bf16),
# поэтому штатный load_encoder_weights не трогаем.
#
# Тонкость: декодер делает tied lm_head ПЛОТНЫМ матмулом `h @ embed_tokens.weight.T`
# (decoder.py), что несовместимо с QuantizedEmbedding. Поэтому embed_tokens НЕ
# квантуем, а ДЕквантуем в плотный bf16-вес (совпадает с dtype остальных тензоров).
#
# Безопасность для не-квантованных (bf16) репо: если в config нет "quantization"
# или в весах нет ни одного .scales — ветка квантизации пропускается и поведение
# идентично оригиналу (strip префикса "model." + load_weights).
import json as _json
from pathlib import Path as _Path
import mlx.core as _mx
import mlx.nn as _nn
import qwen3_asr_mlx.model as _qmodel  # модуль, в котором from_pretrained ищет имя


def _load_decoder_weights_quant_aware(decoder, model_path):
    path = _Path(model_path)
    if not path.is_dir():
        from huggingface_hub import snapshot_download
        path = _Path(snapshot_download(repo_id=str(model_path)))

    raw = _mx.load(str(path / "model.safetensors"))
    prefix = "model."
    weights = {k[len(prefix):]: v for k, v in raw.items() if k.startswith(prefix)}

    quant = None
    cfg_file = path / "config.json"
    if cfg_file.is_file():
        cfg = _json.loads(cfg_file.read_text(encoding="utf-8"))
        quant = cfg.get("quantization") or cfg.get("quantization_config")

    if quant and any(k.endswith(".scales") for k in weights):
        group_size = int(quant["group_size"])
        bits = int(quant["bits"])
        mode = quant.get("mode", "affine")

        # tied lm_head → эмбеддинг оставляем плотным: дексантуем в bf16.
        if "embed_tokens.scales" in weights:
            weights["embed_tokens.weight"] = _mx.dequantize(
                weights["embed_tokens.weight"],
                scales=weights.pop("embed_tokens.scales"),
                biases=weights.pop("embed_tokens.biases"),
                group_size=group_size, bits=bits, mode=mode,
            )

        # Квантуем РОВНО те подмодули, для которых в файле есть .scales.
        def _is_quantized(submodule_path, module):
            return hasattr(module, "to_quantized") and f"{submodule_path}.scales" in weights

        _nn.quantize(decoder, group_size=group_size, bits=bits, mode=mode,
                     class_predicate=_is_quantized)

    decoder.load_weights(list(weights.items()))
    # Принудительная материализация весов (это graph-вычисление MLX, не Python-
    # builtin); паритет с upstream-загрузчиком и ранняя диагностика ошибок.
    getattr(_mx, "eval")(decoder.parameters())


_qmodel.load_decoder_weights = _load_decoder_weights_quant_aware
# ------------------------------------------------------------------------------

model = Qwen3ASR.from_pretrained(model_src)

app = FastAPI()


@app.get("/health")
def health():
    return {"ok": True, "model": args.model, "source": model_src}


@app.post("/transcribe")
async def transcribe(request: Request):
    try:
        lang = request.headers.get("lang") or request.query_params.get("lang") or args.lang
        body = await request.body()
        pcm = np.frombuffer(body, dtype="<f4")
        # language=lang — пин доминирующего языка (ru) + транскрипция (не перевод):
        # английские термины остаются латиницей внутри русской фразы (спека §11).
        result = model.transcribe(pcm, language=lang)
        resp = {"text": getattr(result, "text", "")}
        segs = getattr(result, "segments", None)
        if segs is not None:
            try:
                resp["segments"] = [{"text": getattr(s, "text", str(s))} for s in segs]
            except Exception:
                pass
        return JSONResponse(content=resp)
    except Exception as exc:
        return JSONResponse(status_code=500, content={"error": str(exc)})


if __name__ == "__main__":
    uvicorn.run(app, host="127.0.0.1", port=args.port, log_level="warning")

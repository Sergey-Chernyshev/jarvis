# Third-Party Notices

Jarvis itself is licensed under the **MIT License** (see [`LICENSE`](LICENSE)).
It bundles, links against, and downloads third-party components that are
licensed separately. This file lists the obligations that apply.

> The user-facing summary lives in the [License](README.md#license) section of the
> README (Russian: [«Лицензия»](README.ru.md#лицензия)).

---

## 1. Bundled and redistributed in the app

### MediaRemoteAdapter (`bin/mediaremote-adapter/`)

A precompiled framework (`MediaRemoteAdapter.framework`) and a helper script are
vendored into this repository and shipped inside the `.app`. Licensed under the
**BSD 3-Clause License**; the notice below is reproduced verbatim to satisfy
clause 2 (binary redistribution).

```
BSD 3-Clause License

Copyright (c) 2025, Jonas van den Berg and contributors

Redistribution and use in source and binary forms, with or without
modification, are permitted provided that the following conditions are met:

1. Redistributions of source code must retain the above copyright notice, this
   list of conditions and the following disclaimer.

2. Redistributions in binary form must reproduce the above copyright notice,
   this list of conditions and the following disclaimer in the documentation
   and/or other materials provided with the distribution.

3. Neither the name of the copyright holder nor the names of its
   contributors may be used to endorse or promote products derived from
   this software without specific prior written permission.

THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS IS"
AND ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE
IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE ARE
DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT HOLDER OR CONTRIBUTORS BE LIABLE
FOR ANY DIRECT, INDIRECT, INCIDENTAL, SPECIAL, EXEMPLARY, OR CONSEQUENTIAL
DAMAGES (INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR
SERVICES; LOSS OF USE, DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER
CAUSED AND ON ANY THEORY OF LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY,
OR TORT (INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE
OF THIS SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.
```

---

## 2. Compiled-in Rust crates

Jarvis links many Rust crates. Most are dual-licensed **MIT OR Apache-2.0**;
under that choice Jarvis takes MIT. Two audio crates are **Apache-2.0 only**, so
their `NOTICE` must be preserved when distributing the compiled `.app`:

| Crate | License | Note |
|-------|---------|------|
| `cpal` | Apache-2.0 (only) | Apache-2.0 §4(d): carry forward any `NOTICE`. |
| `hound` | Apache-2.0 (only) | Apache-2.0 §4(d): carry forward any `NOTICE`. |
| `tauri`, `tokio`, `serde`, `axum`, `reqwest`, `rodio`, `ort`, `regex`, `chrono`, … | MIT OR Apache-2.0 | Jarvis elects MIT. |

**Maintainer action for binary releases:** generate a complete bundled-dependency
license file (e.g. `cargo about generate` or `cargo bundle-licenses`) and include
it in the release artifacts. This covers Apache-2.0 `NOTICE` and BSD attribution
for every compiled dependency, including PyTorch (BSD-3-Clause) pulled by the
voice sidecar at runtime.

---

## 3. Downloaded model weights (not bundled)

Jarvis **does not ship** any model weights. They are downloaded at install/first
use into `~/.jarvis/` from their upstream sources. Each weight keeps its own
license, which binds **how you may use** the corresponding feature — independently
of Jarvis's MIT code license.

| Artifact | Used for | License | Commercial use |
|----------|----------|---------|----------------|
| Silero `v4_ru` | **default voice (TTS)** | CC BY-NC-SA 4.0 | ❌ non-commercial |
| Silero `v5_ru` | voice (TTS) | CC BY-NC-SA 4.0 | ❌ non-commercial |
| Silero `v5_cis_base` / `_nostress` | voice (TTS) | MIT | ✅ yes |
| openWakeWord `hey_jarvis_v0.1` | **default wake-word** | CC BY-NC-SA 4.0 | ❌ non-commercial |
| openWakeWord backbones (`melspectrogram`, `embedding_model`) | wake-word / STT front-end | Apache-2.0 | ✅ yes |
| whisper.cpp `ggml-*` weights | speech-to-text | MIT | ✅ yes |
| Qwen3 (via `mlx-community`) | speech-to-text | Apache-2.0 | ✅ yes |

> Licenses verified against upstream sources on 2026-06-22. Note: the Silero repo
> labels its non-commercial models `CC BY-NC 4.0` in the README but ships a
> `CC BY-NC-SA 4.0` `LICENSE` file; the `LICENSE` file is treated as controlling.

### Commercial-clean configuration

The **default** voice (`v4_ru`) and **default** wake-word (`hey_jarvis`) are
**non-commercial**. For commercial use, switch to a fully permissive set:

- **Voice (TTS):** Silero `v5_cis_base` / `v5_cis_base_nostress` (MIT).
- **Wake-word:** train your own model (openWakeWord's training pipeline is
  Apache-2.0) or disable wake-word. The shared backbones are Apache-2.0.
- **Speech-to-text:** whisper.cpp `ggml-*` (MIT) or Qwen3 (Apache-2.0) — both fine.

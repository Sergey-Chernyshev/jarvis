# macOS-дистрибутив Jarvis — Фаза 1 (DMG + подпись + updater + CI) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Собрать из Tauri-приложения Jarvis скачиваемый Mac-дистрибутив: сейчас — рабочий неподписанный universal `.dmg`; плюс проведённые (активируются позже секретами/иконкой) подпись+нотаризация, встроенный updater и CI-релиз на тег.

**Architecture:** Включаем Tauri-бандл (dmg+app, universal). Подпись/нотаризация и updater-подпись управляются переменными окружения — без них сборка идёт неподписанной и локально-обновляемой, с ними CI выпускает подписанный нотаризованный релиз. Иконка — генерируемый плейсхолдер, заменяемый позже одним файлом.

**Tech Stack:** Tauri 2, `@tauri-apps/cli`, `tauri-plugin-updater`, `tauri-action` (GitHub Actions), `notarytool` (через бандлер Tauri), Rust universal target.

**Из области этого плана исключено (Фаза 2, отдельный план):** онбординг-окно первого запуска, вынос `install/uninstall/status` в `src/install/mod.rs`. Здесь только дистрибутив.

**Предусловия по секретам/иконке (приходят позже, план их не блокирует):**
- Apple: `APPLE_CERTIFICATE`, `APPLE_CERTIFICATE_PASSWORD`, `APPLE_SIGNING_IDENTITY`, `APPLE_ID`, `APPLE_PASSWORD`, `APPLE_TEAM_ID`.
- Updater: `TAURI_SIGNING_PRIVATE_KEY`, `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` (ключ генерим в Task 5, приватный храним вне репо).
- Иконка: `src-tauri/icons/icon-source.png` 1024×1024 (в Task 2 — плейсхолдер).

---

## Структура файлов

- Create: `src-tauri/icons/icon-source.png` — исходник иконки (плейсхолдер 1024×1024).
- Create: `src-tauri/entitlements.plist` — entitlements для hardened runtime.
- Create: `.github/workflows/release.yml` — CI: тег `v*` → подписанный нотаризованный релиз.
- Create: `scripts/make-placeholder-icon.py` — генератор плейсхолдер-PNG (stdlib).
- Modify: `package.json` — devDependency `@tauri-apps/cli`, скрипты `tauri`/`bundle`.
- Modify: `src-tauri/tauri.conf.json` — `bundle` (active, targets, icon, macOS, createUpdaterArtifacts) + `plugins.updater`.
- Modify: `src-tauri/Cargo.toml` — зависимость `tauri-plugin-updater`.
- Modify: `src-tauri/src/main.rs` — регистрация updater-плагина + проверка обновлений на старте.
- Modify: `README.md` — раздел «Релиз и установка».

---

### Task 1: Tauri CLI, universal target, npm-скрипты

**Files:**
- Modify: `package.json`

- [ ] **Step 1: Поставить Tauri CLI как devDependency**

Run:
```bash
cd /Users/se.chernyshev/jarvis
npm install -D @tauri-apps/cli@^2
```
Expected: в `package.json` появляется `devDependencies.@tauri-apps/cli`, создаётся `node_modules/.bin/tauri`.

- [ ] **Step 2: Universal — только в CI (локально host arm64)**

Rust здесь из Homebrew (без rustup), кросс-target `x86_64` локально не добавить.
Решение: локально собираем host (arm64) для дымового теста, а **universal**
делает CI (там rustup ставит обе арки — Task 6). Локально ничего добавлять не надо.

Run (только убедиться, что хост — arm64):
```bash
rustc -vV | grep host
```
Expected: `host: aarch64-apple-darwin`.

- [ ] **Step 3: Добавить npm-скрипты `tauri` и `bundle`**

В `package.json` в объект `scripts` добавить (локальный `bundle` — host arm64,
universal собирает CI):
```json
    "tauri": "tauri",
    "bundle": "tauri build"
```

- [ ] **Step 4: Проверить, что CLI видит проект**

Run:
```bash
npm run tauri -- --version
npm run tauri -- info
```
Expected: версия CLI печатается; `info` находит `src-tauri/tauri.conf.json` (раздел «App directory structure» без ошибок).

- [ ] **Step 5: Commit**

```bash
git add package.json package-lock.json
git commit -m "build(tauri): добавить @tauri-apps/cli, universal target, скрипты tauri/bundle"
```

---

### Task 2: Плейсхолдер-иконка и генерация набора

**Files:**
- Create: `scripts/make-placeholder-icon.py`
- Create: `src-tauri/icons/icon-source.png`
- Modify: `src-tauri/icons/` (генерируемый набор)

- [ ] **Step 1: Написать генератор плейсхолдер-PNG (только stdlib)**

Create `scripts/make-placeholder-icon.py`:
```python
#!/usr/bin/env python3
"""Генерит 1024x1024 RGBA PNG: скруглённый квадрат с диагональным градиентом
на прозрачном фоне (форма в духе macOS-иконки). Только стандартная библиотека.
Заменяется реальной иконкой позже — просто положи свой icon-source.png."""
import zlib, struct, sys, math

W = H = 1024
RADIUS = 184            # скругление углов
MARGIN = 40             # поля до края канвы
# градиент: синий -> бирюзовый (не one-note, читаемо на свету и в тёмной теме)
C0 = (37, 99, 235)      # indigo-600
C1 = (13, 148, 136)     # teal-600

def rounded(x, y):
    """True, если пиксель внутри скруглённого квадрата."""
    lo, hi = MARGIN, W - 1 - MARGIN
    if x < lo or x > hi or y < lo or y > hi:
        return False
    dx = min(x - (lo + RADIUS), (hi - RADIUS) - x, 0)
    dy = min(y - (lo + RADIUS), (hi - RADIUS) - y, 0)
    return dx * dx + dy * dy <= RADIUS * RADIUS

def pixel(x, y):
    if not rounded(x, y):
        return (0, 0, 0, 0)
    t = (x + y) / (2 * (W - 1))           # 0..1 по диагонали
    r = round(C0[0] + (C1[0] - C0[0]) * t)
    g = round(C0[1] + (C1[1] - C0[1]) * t)
    b = round(C0[2] + (C1[2] - C0[2]) * t)
    return (r, g, b, 255)

def chunk(typ, data):
    body = typ + data
    return struct.pack(">I", len(data)) + body + struct.pack(">I", zlib.crc32(body) & 0xffffffff)

def main(path):
    raw = bytearray()
    for y in range(H):
        raw.append(0)                      # filter type 0 (None)
        for x in range(W):
            raw += bytes(pixel(x, y))
    sig = b"\x89PNG\r\n\x1a\n"
    ihdr = struct.pack(">IIBBBBB", W, H, 8, 6, 0, 0, 0)   # 8-bit RGBA
    data = sig + chunk(b"IHDR", ihdr) + chunk(b"IDAT", zlib.compress(bytes(raw), 9)) + chunk(b"IEND", b"")
    with open(path, "wb") as f:
        f.write(data)
    print(f"написал {path} ({len(data)} байт)")

if __name__ == "__main__":
    main(sys.argv[1] if len(sys.argv) > 1 else "src-tauri/icons/icon-source.png")
```

- [ ] **Step 2: Сгенерировать исходник иконки**

Run:
```bash
cd /Users/se.chernyshev/jarvis
python3 scripts/make-placeholder-icon.py src-tauri/icons/icon-source.png
sips -g pixelWidth -g pixelHeight src-tauri/icons/icon-source.png
```
Expected: файл создан; `sips` показывает `pixelWidth: 1024`, `pixelHeight: 1024`.

- [ ] **Step 3: Разложить набор иконок через tauri icon**

Run:
```bash
npm run tauri -- icon src-tauri/icons/icon-source.png
ls src-tauri/icons/
```
Expected: появились `icon.icns`, `icon.ico`, `32x32.png`, `128x128.png`, `128x128@2x.png`, набор `Square*Logo.png` и т.п.

- [ ] **Step 4: Commit**

```bash
git add scripts/make-placeholder-icon.py src-tauri/icons/
git commit -m "feat(icons): плейсхолдер-иконка 1024 + сгенерированный набор (tauri icon)"
```

---

### Task 3: Включить бандл и собрать неподписанный DMG

**Files:**
- Modify: `src-tauri/tauri.conf.json`

- [ ] **Step 1: Прописать bundle-секцию**

В `src-tauri/tauri.conf.json` заменить `"bundle": { "active": false }` на:
```json
    "bundle": {
        "active": true,
        "targets": ["app", "dmg"],
        "category": "DeveloperTool",
        "copyright": "© 2026 Jarvis",
        "icon": [
            "icons/32x32.png",
            "icons/128x128.png",
            "icons/128x128@2x.png",
            "icons/icon.icns",
            "icons/icon.ico"
        ],
        "macOS": {
            "minimumSystemVersion": "11.0",
            "entitlements": "entitlements.plist"
        },
        "createUpdaterArtifacts": true
    }
```
Примечание: `signingIdentity`/нотаризация подхватятся из env в CI (Task 6). Без env — сборка неподписанная. `entitlements.plist` создаётся в Task 4; до него сборку с `--target` не запускаем (ниже шаг проверки делает unsigned-сборку уже после Task 4). Чтобы не блокироваться, сейчас только правим конфиг и валидируем JSON.

- [ ] **Step 2: Проверить валидность конфига**

Run:
```bash
python3 -c "import json; json.load(open('src-tauri/tauri.conf.json')); print('JSON ок')"
npm run tauri -- info
```
Expected: «JSON ок»; `info` без ошибок парсинга конфига.

- [ ] **Step 3: Commit**

```bash
git add src-tauri/tauri.conf.json
git commit -m "build(tauri): включить бандл dmg+app (universal, иконки, updater-артефакты)"
```

---

### Task 4: Entitlements + первая реальная сборка DMG

**Files:**
- Create: `src-tauri/entitlements.plist`

- [ ] **Step 1: Создать entitlements.plist**

Create `src-tauri/entitlements.plist`:
```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <!-- Сетевой клиент: updater тянет манифест/артефакты по HTTPS. -->
    <key>com.apple.security.network.client</key>
    <true/>
    <!-- Jarvis спавнит дочерние процессы (claude, tmux, python-сайдкар).
         Под hardened runtime спавн разрешён; здесь явных доп-энтайтлментов
         не требуется. PyTorch грузится в ОТДЕЛЬНОМ venv-python — под
         hardened runtime приложения не попадает. -->
</dict>
</plist>
```

- [ ] **Step 2: Собрать неподписанный DMG (host arm64)**

ВАЖНО (порядок): `createUpdaterArtifacts: true` (Task 3) требует, чтобы в
`tauri.conf.json` уже была секция `plugins.updater` (Task 5 Step 3) и был задан
`TAURI_SIGNING_PRIVATE_KEY` — иначе бандлер падает «plugins > updater doesn't
exist». Поэтому сначала выполни Task 5 Steps 1–4 (Cargo-зависимость + конфиг +
ключ + main.rs), затем эту сборку — она даст и DMG, и updater-артефакты сразу.

Run:
```bash
cd /Users/se.chernyshev/jarvis
export TAURI_SIGNING_PRIVATE_KEY="$(cat ~/.jarvis/jarvis-updater.key)"
export TAURI_SIGNING_PRIVATE_KEY_PASSWORD=""
npm run bundle
```
Expected: сборка завершается успехом; в конце путь к `.dmg`. Предупреждение про отсутствие signing identity — норм (неподписанная сборка).

- [ ] **Step 3: Проверить артефакты**

Run:
```bash
ls -lh src-tauri/target/release/bundle/dmg/*.dmg
lipo -info src-tauri/target/release/bundle/macos/Jarvis.app/Contents/MacOS/jarvis
```
Expected: есть `Jarvis_0.2.0_aarch64.dmg`; `lipo -info` показывает `arm64` (локально host; universal делает CI).

- [ ] **Step 4: Дымовой тест приложения из бандла**

Run:
```bash
open src-tauri/target/release/bundle/macos/Jarvis.app
sleep 3; pgrep -f 'Jarvis.app/Contents/MacOS/jarvis' && echo "запущено"
osascript -e 'tell application "Jarvis" to quit' 2>/dev/null || pkill -f 'Jarvis.app/Contents/MacOS/jarvis'
```
Expected: процесс поднялся (иконка в меню-баре), затем погашен. (Неподписанное локально из Finder может потребовать ПКМ→Открыть — это ожидаемо до нотаризации.)

- [ ] **Step 5: Commit**

```bash
git add src-tauri/entitlements.plist
git commit -m "build(tauri): entitlements для hardened runtime; собирается unsigned universal DMG"
```

---

### Task 5: Updater — плагин, ключ, конфиг, проверка на старте

**Files:**
- Modify: `src-tauri/Cargo.toml`
- Modify: `src-tauri/src/main.rs:42-60`
- Modify: `src-tauri/tauri.conf.json`

- [ ] **Step 1: Добавить зависимость плагина**

В `src-tauri/Cargo.toml` в `[dependencies]` добавить строку рядом с прочими `tauri-plugin-*`:
```toml
tauri-plugin-updater = "2"
```

- [ ] **Step 2: Сгенерировать updater-ключ**

Run:
```bash
cd /Users/se.chernyshev/jarvis
npm run tauri -- signer generate -w ~/.jarvis/jarvis-updater.key
```
Expected: печатает публичный ключ (строка `dW50cnVzdGVk...` в base64) и пишет приватный в `~/.jarvis/jarvis-updater.key`. **Приватный ключ в репозиторий НЕ коммитим**; для CI он пойдёт в secret `TAURI_SIGNING_PRIVATE_KEY`. Скопировать публичный ключ для следующего шага.

- [ ] **Step 3: Прописать plugins.updater в конфиг**

В `src-tauri/tauri.conf.json` добавить верхнеуровневый ключ `plugins` (рядом с `app`/`bundle`):
```json
    "plugins": {
        "updater": {
            "endpoints": [
                "https://github.com/se.chernyshev/jarvis/releases/latest/download/latest.json"
            ],
            "pubkey": "ВСТАВЬ_ПУБЛИЧНЫЙ_КЛЮЧ_ИЗ_ШАГА_2"
        }
    }
```
Примечание: `pubkey` — реальное значение из шага 2 (это не секрет, он публичный). URL endpoint поправить под фактический GitHub-репозиторий, если slug иной.

- [ ] **Step 4: Зарегистрировать плагин и проверку обновлений в main.rs**

В `src-tauri/src/main.rs` после `.plugin(tauri_plugin_clipboard_manager::init())` (строка ~60) добавить:
```rust
        .plugin(tauri_plugin_updater::Builder::new().build())
```
И в `.setup(...)`-замыкании (либо сразу после построения демона на старте) добавить фоновую проверку — вставить рядом с другими `tauri::async_runtime::spawn` в `main.rs`:
```rust
    // updater: тихая проверка на старте; если есть свежий релиз — ставим и просим перезапуск.
    {
        use tauri_plugin_updater::UpdaterExt;
        let handle = app.handle().clone();
        tauri::async_runtime::spawn(async move {
            if let Ok(updater) = handle.updater() {
                if let Ok(Some(update)) = updater.check().await {
                    crate::log::line(&format!("[updater] доступна версия {}", update.version));
                    let _ = update.download_and_install(|_, _| {}, || {}).await;
                }
            }
        });
    }
```
Примечание: точное место вставки — там, где доступен `app: &tauri::App` / `AppHandle` (в `.setup` или сразу после него). Если имя логгера иное — использовать существующий `crate::log::line` (он уже зовётся в main.rs).

- [ ] **Step 5: Собрать и проверить updater-артефакты**

Run:
```bash
cd /Users/se.chernyshev/jarvis
export TAURI_SIGNING_PRIVATE_KEY="$(cat ~/.jarvis/jarvis-updater.key)"
export TAURI_SIGNING_PRIVATE_KEY_PASSWORD=""
npm run bundle 2>&1 | tail -20
ls src-tauri/target/release/bundle/macos/*.tar.gz*
```
Expected: компиляция с плагином проходит; рядом с `.app` появляются `Jarvis.app.tar.gz` и `Jarvis.app.tar.gz.sig`.

- [ ] **Step 6: Commit**

```bash
git add src-tauri/Cargo.toml src-tauri/Cargo.lock src-tauri/tauri.conf.json src-tauri/src/main.rs
git commit -m "feat(updater): tauri-plugin-updater + проверка обновлений на старте + updater-артефакты"
```

---

### Task 6: CI-воркфлоу релиза (активируется секретами)

**Files:**
- Create: `.github/workflows/release.yml`

- [ ] **Step 1: Создать воркфлоу**

Create `.github/workflows/release.yml`:
```yaml
name: release

on:
  push:
    tags:
      - "v*"

jobs:
  macos:
    runs-on: macos-14
    permissions:
      contents: write
    steps:
      - uses: actions/checkout@v4

      - name: Rust toolchain (+ x86_64 для universal)
        uses: dtolnay/rust-toolchain@stable
        with:
          targets: aarch64-apple-darwin,x86_64-apple-darwin

      - name: Cargo cache
        uses: swatinem/rust-cache@v2
        with:
          workspaces: src-tauri

      - uses: actions/setup-node@v4
        with:
          node-version: 20

      - name: Install deps
        run: npm ci

      - name: Build, sign, notarize, release
        uses: tauri-apps/tauri-action@v0
        env:
          GITHUB_TOKEN: ${{ secrets.GITHUB_TOKEN }}
          # Подпись + нотаризация (Developer ID)
          APPLE_CERTIFICATE: ${{ secrets.APPLE_CERTIFICATE }}
          APPLE_CERTIFICATE_PASSWORD: ${{ secrets.APPLE_CERTIFICATE_PASSWORD }}
          APPLE_SIGNING_IDENTITY: ${{ secrets.APPLE_SIGNING_IDENTITY }}
          APPLE_ID: ${{ secrets.APPLE_ID }}
          APPLE_PASSWORD: ${{ secrets.APPLE_PASSWORD }}
          APPLE_TEAM_ID: ${{ secrets.APPLE_TEAM_ID }}
          # Подпись updater-артефактов
          TAURI_SIGNING_PRIVATE_KEY: ${{ secrets.TAURI_SIGNING_PRIVATE_KEY }}
          TAURI_SIGNING_PRIVATE_KEY_PASSWORD: ${{ secrets.TAURI_SIGNING_PRIVATE_KEY_PASSWORD }}
        with:
          args: --target universal-apple-darwin
          tagName: ${{ github.ref_name }}
          releaseName: "Jarvis ${{ github.ref_name }}"
          releaseDraft: true
          prerelease: false
          includeUpdaterJson: true
```

- [ ] **Step 2: Проверить синтаксис YAML**

Run:
```bash
python3 -c "import yaml,sys; yaml.safe_load(open('.github/workflows/release.yml')); print('YAML ок')" 2>/dev/null \
  || ruby -ryaml -e "YAML.load_file('.github/workflows/release.yml'); puts 'YAML ок'"
```
Expected: «YAML ок». (Если ни python-yaml, ни ruby нет — пропустить, GitHub проверит при пуше.)

- [ ] **Step 3: Commit**

```bash
git add .github/workflows/release.yml
git commit -m "ci: релиз на тег v* через tauri-action (подпись+нотаризация+updater-манифест)"
```

---

### Task 7: README — раздел «Релиз и установка» + список секретов

**Files:**
- Modify: `README.md`

- [ ] **Step 1: Добавить раздел в README**

В `README.md` перед разделом «## Что куда пишется» добавить:
```markdown
## Установка из релиза

1. Скачай `Jarvis_x.y.z_universal.dmg` со страницы releases.
2. Открой DMG, перетащи **Jarvis** в **Applications**, запусти из Launchpad.
3. При первом запуске Jarvis предложит доустановить интеграцию с Claude Code и
   голос (Фаза 2). Пока её нет — поставь интеграцию из исходников: `npm run setup`.

Обновления приложение проверяет само (встроенный updater).

## Релиз (для мейнтейнера)

Сборка/подпись/нотаризация/публикация автоматизированы в GitHub Actions
(`.github/workflows/release.yml`), триггер — пуш тега:

```bash
git tag v0.2.1
git push origin v0.2.1
```

CI соберёт universal DMG, подпишет Developer ID, нотаризует и создаст **черновик**
релиза с DMG, `.app.tar.gz(.sig)` и `latest.json`. Опубликуй черновик вручную.

Нужные **GitHub Secrets** (Settings → Secrets and variables → Actions):

- `APPLE_CERTIFICATE` — Developer ID Application `.p12`, закодированный base64
  (`base64 -i cert.p12 | pbcopy`).
- `APPLE_CERTIFICATE_PASSWORD` — пароль от `.p12`.
- `APPLE_SIGNING_IDENTITY` — имя identity, напр. `Developer ID Application: Имя (TEAMID)`.
- `APPLE_ID` — Apple ID для нотаризации.
- `APPLE_PASSWORD` — app-specific password этого Apple ID.
- `APPLE_TEAM_ID` — Team ID.
- `TAURI_SIGNING_PRIVATE_KEY` — содержимое `~/.jarvis/jarvis-updater.key`.
- `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` — пароль ключа (пустой, если не задавал).

Локальная проверка подписи/нотаризации без CI — те же переменные в окружении
плюс `npm run bundle`.
```

- [ ] **Step 2: Проверить, что Markdown не побит и раздел на месте**

Run:
```bash
grep -n "## Релиз (для мейнтейнера)" README.md && grep -n "APPLE_TEAM_ID" README.md
```
Expected: обе строки находятся.

- [ ] **Step 3: Commit**

```bash
git add README.md
git commit -m "docs(readme): разделы установки из релиза и релизного процесса (секреты)"
```

---

## Самопроверка плана

**Покрытие спеки (Фаза 1):**
- Бандл-конфиг + universal + иконки → Task 1–3.
- Подпись/нотаризация (entitlements + env-gated) → Task 4 + Task 6.
- Updater (плагин, ключ, манифест, проверка) → Task 5.
- CI на тег → Task 6.
- Документация секретов → Task 7.
- Локальный быстрый цикл → `npm run bundle` (Task 4/5), отдельный `scripts/release-local.sh` из спеки опущен как избыточный: `npm run bundle` + env-переменные уже дают локальную подпись; вынесем в скрипт только если понадобится.

**Плейсхолдеры:** все code-блоки конкретны; единственное подставляемое значение — публичный updater-ключ (его реально генерим в Task 5 Step 2) и URL репозитория в endpoint.

**Согласованность:** имена секретов в Task 6 и Task 7 совпадают; `createUpdaterArtifacts` (Task 3) + ключ (Task 5) + `includeUpdaterJson` (Task 6) — единая цепочка updater.

**Phase 2 (онбординг)** — отдельный план после приёмки Фазы 1.
```

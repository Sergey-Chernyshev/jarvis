# Спека: переработка документации Jarvis (README ×2 + LICENSE)

**Дата:** 2026-06-22
**Статус:** на ревью владельца
**Scope этого захода:** красивый двуязычный README (RU-канон + EN) + LICENSE + сопутствующие
правки лицензионных метаданных. CONTRIBUTING / SECURITY / docs-сайт — вне этого захода.

---

## 1. Зачем

Текущий `README.md` — отличный **инженерный лог** (глубокий, честный), но не
**продуктовая страница**: один абзац-вступление на три строки, ноль визуалов для
визуального продукта, нет английского, **нет файла LICENSE** (репозиторий
юридически «все права защищены», GitHub не показывает плашку лицензии). Цель —
переписать витрину под open-source-продукт: понятно, красиво, на двух языках, без
воды, с явной бизнес-проблемой — сохранив всю существующую глубину (прячем её в
`<details>`, не выкидываем).

## 2. Решения владельца (зафиксированы)

| Решение | Выбор |
|---|---|
| Позиционирование | **Open-source-продукт** (зовём и пользователей, и контрибьюторов) |
| Лицензия | **MIT** |
| Объём захода | **README + LICENSE** (+ мелкие метаданные, + THIRD-PARTY-NOTICES) |
| Языки | **RU — основной** (`README.md`), **EN — вторичный** (`README.en.md`) |
| Копирайт | **Полное имя автора** (см. §11 — ожидается точное написание) |
| NC-веса | **Документировать честно + дать commercial-clean рецепт** (факты проверены, см. §6) |
| Визуалы | **Плейсхолдеры + инструкция по съёмке** (скриншот/GIF не могу снять сам) |

## 3. Что доставляем

| Файл | Назначение | Приоритет |
|---|---|---|
| `LICENSE` | MIT, одна строка копирайта, год 2026 | must |
| `README.md` | RU, канон, полный rewrite в продуктовую форму | must |
| `README.en.md` | EN, идентичный порядок секций + баннер «канон = RU» | must |
| `docs/assets/` | плейсхолдеры hero-визуала + `README.md` с инструкцией по съёмке | must |
| `THIRD-PARTY-NOTICES.md` | BSD-3 атрибуция mediaremote-adapter + таблица весов/NOTICE | must |
| `src-tauri/Cargo.toml` | добавить `license = "MIT"` (сейчас отсутствует) | mini-edit |
| `package.json` | добавить `"license": "MIT"` (сейчас отсутствует) | mini-edit |

**Сознательно пропускаем:** CONTRIBUTING, SECURITY, CHANGELOG, CODE_OF_CONDUCT,
issue/PR-шаблоны, docs-сайт, OpenSSF-бейджи — overkill для соло-проекта на этом этапе.

## 4. Бизнес-проблема (черновик копи, оба языка)

**RU:**
> Когда ты гоняешь несколько агентов Claude Code разом, **узким местом
> становишься ты сам**. Сессии раскиданы по терминалам, вкладкам и Spaces — и с
> одного взгляда не видно, какой агент **встал на запросе разрешения**, какой
> **уже закончил и простаивает**, сжигая твоё время, а какой **упёрся в лимит**.
> Родные уведомления Claude Code — по одной сессии и привязаны к терминалу (в
> расширении для VS Code их вообще нет), единой картины по всем сессиям нет. А
> уснувший мак **молча морозит** claude-процессы и рвёт API-запросы, убивая
> долгие ночные прогоны — и на Apple Silicon закрытая крышка форсит сон, который
> обычный `caffeinate` не обходит.

**EN:**
> Running many Claude Code agents at once turns **you** into the bottleneck.
> Spread across terminals, tabs and macOS Spaces, you can't see at a glance which
> agent is **blocked on a permission prompt**, which **finished and is now idle**
> burning your wall-clock, and which **hit a rate limit**. Claude Code's native
> notifications are per-session and terminal-scoped (they don't even fire in the
> VS Code extension), with no single pane for the aggregate state. And a sleeping
> Mac **silently freezes** claude processes and severs in-flight requests,
> killing long overnight runs — and on Apple Silicon, closing the lid forces a
> sleep that plain `caffeinate` can't bypass.

**Value prop (одна строка):**
- RU: «Видь, слышь и отвечай каждому агенту Claude Code из меню-бара — чтобы ни один не простаивал в ожидании тебя, а уснувший мак не убивал долгий прогон.»
- EN: «See, hear, and reply to every Claude Code agent from your menu bar — so no agent stalls waiting on you, and a sleeping Mac never kills a long run.»

**Позиционирование (важно):** Jarvis — это **mission-control: монитор + пульт, не
оркестратор**. Он не плодит агентов и не владеет планом; он дополняет Claude
Squad / Conductor / Crystal и те сессии, что ты уже запустил. Доска задач —
read-only. Это снимает заведомо проигрышную битву с оркестраторами и совпадает с
реальной границей кода.

**Целевой пользователь:** соло-разработчик / маленькая команда на macOS, кто уже
**перешёл порог боли в 3+ параллельных сессии** и держит ~10–20 агентных
потоков. Power-user-продукт, не для новичка. RU-first упаковка (README + голос)
сигналит русскоязычную основную аудиторию с глобальным вторым кругом.

## 5. Структура README (14 секций; одинаковый порядок в обоих файлах)

| # | Секция | Суть |
|---|---|---|
| 0 | Переключатель языка | Первая строка обоих файлов; нативные автонимы; **относительные** ссылки; текущий язык — жирным/без ссылки |
| 1 | Hero | Имя + **одна** строка-tagline (см. §5.1), не трёхстрочный абзац |
| 2 | Бейджи | Один ряд, 3–4 шт.: release/version · MIT · macOS universal · (опц.) статус релизного workflow «на тегах». Без vanity (звёзды/загрузки) и без вводящего generic-build (CI идёт только на тегах) |
| 3 | Hero-визуал | Один скриншот/GIF сразу под бейджами (плейсхолдер этого захода) |
| 4 | Проблема | 3–4 предложения боли **до** фич (копи §4) |
| 5 | Highlights | 8–10 буллетов, лид жирным существительным (см. §5.2) |
| 6 | Позиционирование | 1–2 строки: монитор+пульт, не оркестратор |
| 7 | Установка | **DMG-first** (3 шага) → строка Requirements → «Сборка из исходников» ниже/в `<details>` |
| 8 | Фичи | На каждую: H2 + emoji + жирное имя, 1 строка ценности, 3–5 буллетов, `<details>Как это работает</details>` |
| 9 | Сравнение | Таблица: Jarvis vs Claude Code native vs мониторы vs оркестраторы (см. §5.3) |
| 10 | Архитектура | Существующая ASCII-схема + поток событий хуков — **перенести сюда, ниже фич**; trust-месседж (no scraping / no telemetry / `~/.jarvis` / teardown одной командой) на первый план; плумбинг — в `<details>` |
| 11 | Статус / Roadmap | Честные ограничения переформулировать; STT — beta, **wake-word и agent-chat — experimental/in-progress** |
| 12 | Версии | 1 строка: pre-1.0 (SemVer 0.x), контракт хуков и форматы `~/.jarvis` могут меняться; 1.0 заморозит контракт |
| 13 | Вклад | 1 строка-указатель: «Issues и PR — велкам». Ссылка на `docs/` как Documentation |
| 14 | Лицензия | MIT + таблица весов/коммерческого статуса + NC-оговорка + BSD-3 атрибуция + трейдмарк-дисклеймер (см. §6, §7) |

### 5.1 Tagline
- RU: «Меню-бар для macOS, который следит за всеми сессиями Claude Code разом — и говорит, когда ты нужен.»
- EN: «A macOS menu-bar command center for every Claude Code agent at once — it tells you the moment one needs you.»

### 5.2 Highlights (черновик, оба языка; порядок одинаковый)
1. **Мульти-сессионный монитор в меню-баре** — все сессии Claude Code во всех терминалах, живые счётчики ⏸ ждут / ⚙ работают. · **Multi-session menu-bar monitor** — every session across every terminal, live ⏸ waiting / ⚙ working counters.
2. **Тосты поверх фуллскрина** — «нужно разрешение» видно даже в полноэкранном приложении. · **Toasts that render over fullscreen.**
3. **Панель ⌘J, always-on-top (как Raycast)** — открыл, глянул, сделал, закрыл; фокус не ворует. · **Always-on-top ⌘J panel.**
4. **Ответ прямо в сессию** — печатаешь в сессию через tmux, даже если окно свёрнуто или на другом Space. · **Reply into any session via tmux.**
5. **Пульт: модель и effort** — Opus/Sonnet/Haiku и уровень рассуждений из панели. · **Remote-control model & reasoning effort.**
6. **Jarvis говорит** — локальный Silero TTS озвучивает по-русски, что сессия сделала/чего ждёт. · **Jarvis speaks — local TTS summaries.**
7. **Голосовой ввод (диктовка)** — push-to-talk (F8), STT вставляет расшифровку в активную сессию. · **Voice dictation (push-to-talk STT).**
8. **Не давать маку уснуть** — анти-сон (аналог caffeinate) + страхуемый clamshell-режим для ночных прогонов. · **Keep the Mac awake — anti-sleep + guarded clamshell mode.**
9. **Read-only доска задач** — живой прогресс TodoWrite (готово/в работе/в очереди) по сессии. · **Read-only TodoWrite task board.**
10. **Событийно и приватно** — на родных хуках Claude Code (без чтения экрана), всё локально, без телеметрии. · **Event-driven & private — built on hooks, no screen scraping, no telemetry.**

> Пометки честности: **wake-word** — за фича-флагом `wakeword-ort`, по умолчанию
> инертен → раздел Roadmap, ярлык *experimental*. **Agent-chat (capability
> platform)** — MCP-мост есть, UI не зашипан → Roadmap.

### 5.3 Таблица сравнения (колонки)
Строки: **Jarvis** · мониторы меню-бара (напр. c9watch) · оркестраторы
(Conductor / Crystal / Claude Squad) · Claude Code native.
Колонки: детект (hooks vs process-scan vs native) · единый меню-бар по всем
сессиям · ответ-в-сессию · пульт модель/effort · голос/TTS · анти-сон · clamshell
· read-only доска задач · цена настройки · лицензия.
4 уникальные клетки Jarvis: hooks/no-scraping · reply+steer · TTS · clamshell.

## 6. Лицензия и веса (факты проверены по первоисточникам 2026-06-22)

**Код Jarvis — MIT.** Веса моделей **скачиваются** на установке (не коммитятся,
не бандлятся) → клаузула *redistribution* CC-NC не задевается, но клаузула *use*
**связывает конечного пользователя** дефолтных фич. Это надо раскрыть.

| Артефакт | Лицензия | Коммерч.? | Источник |
|---|---|---|---|
| Silero **`v4_ru`** (дефолт-голос) | CC BY-NC-SA 4.0 | **Нет** | snakers4/silero-models |
| Silero `v5_ru` | CC BY-NC-SA 4.0 | **Нет** | snakers4/silero-models |
| Silero **`v5_cis_base` / `_nostress`** | **MIT** | **Да** | snakers4/silero-models |
| openWakeWord **`hey_jarvis_v0.1.onnx`** | CC BY-NC-SA 4.0 | **Нет** | dscripka/openWakeWord |
| openWakeWord backbones (`melspectrogram`, `embedding_model`) | Apache-2.0 | Да | dscripka/openWakeWord |
| whisper.cpp ggml-веса | MIT | Да | ggerganov/whisper.cpp (HF) |
| Qwen3 (mlx-community) | Apache-2.0 | Да | Qwen / mlx-community (HF) |
| PyTorch (runtime сайдкара) | BSD-3-Clause | Да | pytorch/pytorch |
| crate `cpal` | Apache-2.0 **only** | Да | RustAudio/cpal |
| crate `hound` | Apache-2.0 **only** | Да | ruuda/hound |

**Вывод:** дефолтная конфигурация **НЕ commercial-clean** (Silero `v4_ru` +
`hey_jarvis` — non-commercial). **Commercial-clean рецепт** (задокументировать как
опцию): голос → Silero `v5_cis_base` (MIT); wake-word → обучить свой или выключить
(backbones Apache-2.0 — ок); STT → Whisper (MIT) или Qwen3 (Apache-2.0).
**NOTICE-долг:** `cpal`, `hound`, openWakeWord-backbones, Qwen3 — Apache-2.0 §4
требует пронести NOTICE в `.app`; PyTorch — BSD-3 атрибуция. Сгенерировать при
сборке (`cargo about` / `cargo-bundle-licenses`) и вложить в бандл/релиз.

**LICENSE-файл:** стандартный текст MIT с choosealicense.com, ровно одна строка
`Copyright (c) 2026 <COPYRIGHT_HOLDER>` (имя — реальное физлицо, не «Jarvis», не
корпоративная почта; см. §11).

## 7. THIRD-PARTY-NOTICES.md

1. **mediaremote-adapter** (`bin/mediaremote-adapter/`, **вкомпилен в `.app`**,
   бинарь в git) — BSD-3-Clause, воспроизвести дословно:
   `Copyright (c) 2025, Jonas van den Berg and contributors` + текст условий +
   disclaimer (BSD-3 §2 для binary redistribution + §3 no-endorsement).
2. **Таблица весов** из §6 (что non-commercial, что commercial-clean).
3. Пометка про сборочный NOTICE (Apache-2.0 деп) — генерить при бандле.

## 8. Трейдмарк / неаффилиация (в оба README, footer)
- RU/EN: Jarvis — независимый open-source-проект, **не аффилирован с Anthropic,
  PBC**; «Claude»/«Claude Code» — товарные знаки Anthropic (CLAUDE — рег.,
  USPTO #7645254), упомянуты **номинативно** (совместимость). Также **не**
  аффилирован с Marvel/Disney; сходство с вымышленным «J.A.R.V.I.S.» —
  непреднамеренное.
- Использовать «Claude Code» только описательно; не лого-стилизацией; не пихать
  «Claude» в имя пакета/продукта. Без Iron Man / Stark-имиджа и точечного
  написания «J.A.R.V.I.S.». Перед публикацией свериться с brand-guidelines
  Anthropic.
- Одна строка, что Jarvis работает поверх **официальных хуков** Claude Code (не
  OAuth-piggyback) — чтобы не пересекаться с ToSами Anthropic по сторонним тулзам.

## 9. Двуязычная раскладка и анти-дрейф
- Плоский корень: `README.md` (RU, канон, единственный, что GitHub рендерит на
  главной) + `README.en.md` (EN). Русский файл **не переименовывать**.
- Переключатель — **первая строка** обоих файлов:
  - `README.md`: `<p align="center"><b>Русский</b> · <a href="README.en.md">English</a></p>`
  - `README.en.md`: `<p align="center"><a href="README.md">Русский</a> · <b>English</b></p>`
- В `README.en.md` (только) — баннер у верха: «> This is an English translation.
  The canonical version is the Russian [README.md](README.md); if they diverge,
  the Russian version is authoritative.»
- **Не переводить никогда:** код-фенсы, CLI-команды (`npm run setup`, `cargo run`),
  пути (`~/.jarvis/`, `src-tauri/`), URL бейджей, имена событий хуков
  (`SessionStart`, `Stop`, `Notification`) — байт-в-байт.
- **Дисциплина синка:** правим `README.md` первым, зеркалим в `README.en.md` в
  том же коммите/PR. Один общий `docs/assets/` на оба файла. (Опц. позже —
  CI-страж, предупреждающий, если RU менялся, а EN нет.)

## 10. Style guide (без воды)
- Tagline — одно предложение 4–12 слов, лид «что это + одна суперсила».
- Установка — рано (блок 2–3), DMG-first; from-source — ниже.
- Ровно один hero-визуал над сгибом (для GUI это обязательно).
- Одна мысль — один буллет; лид жирным существительным; команды — в код-блоках,
  не прозой; глубина — в `<details>`.
- Короткие параллельные H2 (чтобы авто-TOC GitHub читался); **руками TOC не вести**.
- Бейджи: мало, осмысленно, один ряд, каждый — правдив и кликабелен; без
  vanity (звёзды/загрузки) и без вводящих в заблуждение (generic build при CI
  только на тегах).
- Честность про ограничения и границы — это сигнал доверия для power-аудитории.
- Без маркетингового пуфа и emoji в теле прозы; решительные глаголы.
- Два языковых файла структурно идентичны; непереводимые токены — байт-в-байт.

## 11. Открытые зависимости (нужны до записи финала)
1. **Точное написание полного имени** для строки копирайта (Latin / Cyrillic /
   оба; имя ± email). До получения — плейсхолдер `<COPYRIGHT_HOLDER>`.
2. **Hero-визуал** — скриншот/GIF снять не могу. Этого захода: `docs/assets/` с
   плейсхолдер-ссылками + `docs/assets/README.md` с инструкцией (что снять:
   ⌘J-панель, счётчики меню-бара, тост поверх фуллскрина; GIF 5–15с @ 10–15fps).
3. **Пустая страница releases.** Ссылка на DMG в README будет битой, пока не
   вырезан реальный тег `v0.2.0`. Отметить в README или подождать релиз.

## 12. Порядок сборки (план исполнения — после апрува спеки)
1. `LICENSE` (MIT, плейсхолдер/имя) + `Cargo.toml`/`package.json` метаданные.
2. `THIRD-PARTY-NOTICES.md` (BSD-3 дословно + таблица весов).
3. `docs/assets/` + инструкция по съёмке (плейсхолдеры).
4. `README.md` (RU, канон) — полный rewrite по §5.
5. `README.en.md` (EN) — зеркало RU, тот же порядок, баннер канона.
6. Самопроверка: ссылки переключателя, относительность путей, непереводимые
   токены байт-в-байт, согласованность версий (README vs `Cargo.toml`:
   `rust-version` сейчас 1.77.2, в старом README было «1.88+» — выверить).
7. Коммит.

---

### Замечание по процессу
Брейншторм-навык предполагает отдельный «implementation plan» после спеки. Для
доставки из 3 markdown-файлов это лишняя бюрократия (владелец просил «без воды»):
план сложён в §12 этой спеки. После апрува спеки и получения имени — исполняем §12
напрямую.

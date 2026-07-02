<p align="center"><a href="CONTRIBUTING.md">English</a> · <b>Русский</b></p>

# Как внести вклад в Jarvis

Спасибо за интерес к проекту! Jarvis — это mission-control в меню-баре macOS для сессий Claude Code и Codex CLI (Rust + Tauri). Любой вклад приветствуется: баг-репорты, идеи, документация, код.

> Участвуя, ты соглашаешься соблюдать [Кодекс поведения](CODE_OF_CONDUCT.md).

## С чего начать

- **Нашёл баг?** Открой [issue](https://github.com/Sergey-Chernyshev/jarvis/issues/new/choose) по шаблону «Баг».
- **Есть идея?** Открой issue по шаблону «Предложение» — обсудим, прежде чем писать код.
- **Хочешь взяться за задачу?** Загляни в [issues](https://github.com/Sergey-Chernyshev/jarvis/issues), особенно с метками `good first issue` и `help wanted`. Напиши в issue, что берёшь её.

Для крупных изменений **сначала открой issue** и согласуй подход — так ты не потратишь время на то, что не вмёржат.

## Требования к окружению

- **macOS 11+** (проект macOS-only — Tauri-приложение меню-бара).
- **Rust** (stable) — установи через [rustup](https://rustup.rs/).
- **Node.js 20+** и npm.
- **CMake** — нужен для сборки `whisper.cpp` (фича `whisper-native`): `brew install cmake`.
- **tmux** (опционально) — нужен для ответа-в-сессию и пульта: `brew install tmux`.

```bash
git clone https://github.com/Sergey-Chernyshev/jarvis.git
cd jarvis
npm ci
```

## Сборка и запуск

```bash
npm start          # собрать (release, все features), подписать ad-hoc и запустить dev-профиль (~/.jarvis-dev)
npm test           # cargo test
```

Под капотом `npm start` собирает бинарь с фичами `wakeword-ort,whisper-native,stt-vad` и подписывает его ad-hoc-подписью (нужно для доступа к микрофону на macOS). Запуск идёт на отдельном dev-профиле (`~/.jarvis-dev`), так что твоя боевая установка не затрагивается. Полный список команд — в `package.json` (`setup`, `teardown`, `status`, `bundle`, `start:prod`).

> **Веса моделей:** дефолтные веса TTS/wake-word распространяются под некоммерческими лицензиями (CC BY-NC-SA). Код проекта — MIT. Подробности в [THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md).

## Стиль кода

- **Clippy и тесты — блокирующие:** CI требует чистый `cargo clippy` и зелёный `cargo test`.
- **`cargo fmt` — только информационный.** Проект использует компактный авторский стиль, не совпадающий с дефолтным rustfmt — CI показывает расхождения, но не блокирует. **Не переформатируй файлы массово**; держи диффы минимальными и пиши в стиле окружающего кода.
- **Комментарии и текст UI** — сейчас на русском; пиши в стиле окружающего кода.
- Следуй существующим паттернам: смотри, как устроены соседние файлы, и пиши так же.

## Коммиты и Pull Request'ы

- **Сообщения коммитов** — в формате [Conventional Commits](https://www.conventionalcommits.org/): `feat(stt): …`, `fix(convo): …`, `docs: …`, `chore: …`. Текст — на английском или русском, как тебе удобнее (история проекта в основном на русском).
- **Ветки** создавай от `master`: `feat/<кратко>`, `fix/<кратко>`.
- Прямой push в `master` закрыт — изменения вливаются **только через Pull Request** с зелёным CI.
- В PR:
  - заполни шаблон (что и зачем);
  - убедись, что **CI зелёный** (`cargo clippy`, `cargo test`);
  - держи PR сфокусированным — одна логическая задача на PR;
  - **двуязычная документация:** канон — английский (`README.md`, `CONTRIBUTING.md`). Правишь английский док — зеркаль изменение в русскую версию (`README.ru.md`, `CONTRIBUTING.ru.md`) тем же PR.

## Безопасность

Не открывай публичные issue по уязвимостям. Как сообщить приватно — см. [SECURITY.md](SECURITY.md).

## Лицензия

Внося вклад, ты соглашаешься, что он будет распространяться под лицензией [MIT](LICENSE).

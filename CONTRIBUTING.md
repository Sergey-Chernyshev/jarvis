<p align="center"><b>Русский</b> · <a href="CONTRIBUTING.en.md">English</a></p>

# Как внести вклад в Jarvis

Спасибо за интерес к проекту! Jarvis — это меню-бар для macOS, который следит за сессиями Claude Code (Rust + Tauri). Любой вклад приветствуется: баг-репорты, идеи, документация, код.

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

Под капотом `npm start` собирает бинарь с фичами `wakeword-ort,whisper-native,stt-vad` и подписывает его ad-hoc-подписью (нужно для доступа к микрофону на macOS). Полный список команд — в `package.json` (`setup`, `status`, `bundle` и т.д.).

> **Веса моделей:** дефолтные веса STT/wake-word распространяются под некоммерческими лицензиями (CC BY-NC-SA). Код проекта — MIT. Подробности в [THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md).

## Стиль кода

- **Rust:** перед коммитом прогони `cargo fmt` и `cargo clippy`. CI проверяет оба.
- **Комментарии и текст UI** — на русском (проект русскоязычный), как в окружающем коде.
- Следуй существующим паттернам: смотри, как устроены соседние файлы, и пиши в том же стиле.

## Коммиты и Pull Request'ы

- **Сообщения коммитов** — в формате [Conventional Commits](https://www.conventionalcommits.org/): `feat(stt): …`, `fix(convo): …`, `docs: …`, `chore: …`. Текст — на русском, как в истории проекта.
- **Ветки** создавай от `master`: `feat/<кратко>`, `fix/<кратко>`.
- Прямой push в `master` закрыт — изменения вливаются **только через Pull Request**.
- В PR:
  - заполни шаблон (что и зачем);
  - убедись, что **CI зелёный** (`cargo fmt --check`, `clippy`, `cargo test`);
  - держи PR сфокусированным — одна логическая задача на PR;
  - двуязычная документация: правишь русскую версию (`README.md`, `CONTRIBUTING.md`) — зеркаль в `*.en.md` тем же PR.

## Безопасность

Не открывай публичные issue по уязвимостям. Как сообщить приватно — см. [SECURITY.md](SECURITY.md).

## Лицензия

Внося вклад, ты соглашаешься, что он будет распространяться под лицензией [MIT](LICENSE).

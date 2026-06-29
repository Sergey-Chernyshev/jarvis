---
name: release-jarvis
description: Выпустить новый релиз Jarvis (macOS, Tauri) — поднять версию, собрать ad-hoc-подписанный .dmg через GitHub Actions (без Apple Developer), опубликовать draft-релиз и обеспечить работу авто-обновления. Использовать при словах «катить релиз», «новый релиз», «зарелизь», «bump версию», «выпусти обновление».
---

# Релиз Jarvis

Jarvis — это Tauri-приложение под macOS. Релиз = тег `vX.Y.Z` → GitHub Actions
(`.github/workflows/release.yml`) собирает **ad-hoc-подписанный** `.dmg` (без
Apple Developer) + артефакты авто-апдейтера и создаёт **draft**-релиз.
Публикация — вручную.

## TL;DR (happy path)

1. Версия: реши номер по semver (см. «Политика версий»).
2. Подними версию в **4 файлах** через PR (прямой пуш в master закрыт protection):
   `package.json`, `src-tauri/tauri.conf.json`, `src-tauri/Cargo.toml`,
   `src-tauri/Cargo.lock` (секция `[[package]] name = "jarvis"`).
3. Смержи PR (squash), дождавшись чека `fmt · clippy · test`.
4. Поставь тег на свежий master и запушь:
   ```bash
   git fetch origin
   git tag vX.Y.Z origin/master
   git push origin vX.Y.Z
   ```
5. Тег запускает `release.yml`. Дождись `completed/success`:
   ```bash
   gh run list --workflow=release.yml --limit 1
   ```
6. Проверь draft-релиз и опубликуй:
   ```bash
   gh release view vX.Y.Z --json isDraft,assets
   gh release edit vX.Y.Z --draft=false   # публикация (когда готов)
   ```

## Предусловия и подводные камни

- **Workflow может быть выключен.** `release.yml` бывает в состоянии
  `disabled_manually` — тогда пуш тега ничего не запустит. Проверь и включи:
  ```bash
  gh api repos/Sergey-Chernyshev/jarvis/actions/workflows --jq '.workflows[]|select(.path|test("release"))|.state'
  gh workflow enable release.yml
  ```
- **Сборка только aarch64 (Apple Silicon).** В `release.yml` стоит
  `--target aarch64-apple-darwin`. НЕ ставить `universal-apple-darwin`: у проекта
  второй бинарь `jarvis-setup` (`[[bin]]`), который tauri не лило для universal →
  бандл падает с `Failed to copy binary ... jarvis-setup does not exist`.
  Intel-сборка = отдельная задача (нужно отдельно лило все бинари).
- **Подпись — ad-hoc, без Apple Developer.** В `tauri.conf.json`
  `macOS.signingIdentity: "-"`; Apple-сертификата у проекта НЕТ (ни в CI, ни
  локально). Workflow НЕ должен требовать `APPLE_*` секретов — их не существует,
  сборка упадёт на codesign. Минус: Gatekeeper при первом запуске скажет
  «неустановленный разработчик» → пользователь ПКМ→Открыть. Нотаризации нет.
- **Ключ апдейтера (независим от Apple):** секрет `TAURI_SIGNING_PRIVATE_KEY`
  должен соответствовать `plugins.updater.pubkey` в `tauri.conf.json`. Уже
  настроено (ключ сгенерён `tauri signer generate`, пароль пустой). Иначе подпись
  `latest.json` не сойдётся и клиенты не примут апдейт. Перегенерация:
  `node_modules/.bin/tauri signer generate -w /tmp/k -p "" --ci`, затем
  `gh secret set TAURI_SIGNING_PRIVATE_KEY < /tmp/k` и обнови pubkey в конфиге.
- **Endpoint апдейтера** в `tauri.conf.json` обязан указывать на реальный репо:
  `https://github.com/Sergey-Chernyshev/jarvis/releases/latest/download/latest.json`
  (исторически был placeholder `OWNER/REPO` — из-за него апдейты не резолвились).
- **Branch protection строгий:** требуется чек `fmt · clippy · test` И ветка
  должна быть up-to-date с master. Если PR «BEHIND» — обнови серверным мержем,
  не локальным ребейзом (репо параллельно ресетят, см. ниже):
  ```bash
  gh api repos/Sergey-Chernyshev/jarvis/pulls/<N>/update-branch -X PUT
  ```
  Затем дождись перепрогона CI и мержи. Чек `review` (Claude Code Review) часто
  падает транзиентно и **не блокирует** мерж (не required).
- **git-churn в рабочем каталоге.** Каталог иногда параллельно ресетят на
  `origin/master` (мерж чужих PR). Не держи важную работу только в локальной
  ветке — пушь в origin сразу; при многошаговых правках якори тегом. См.
  память `jarvis-shared-workdir-git-churn`.

## Если сборка упала

1. Посмотри причину:
   ```bash
   gh run view <run-id> --log-failed | tail -40
   ```
2. Почини на master через PR.
3. Перенеси тег на исправленный коммит и перезапусти сборку:
   ```bash
   git push origin :refs/tags/vX.Y.Z      # удалить тег на remote
   git fetch origin
   git tag -f vX.Y.Z origin/master
   git push origin vX.Y.Z                  # повторный пуш = повторный запуск
   ```
   (Пока релиз не опубликован, переиспользовать тот же тег безопасно.)

## Что делает workflow

`tauri-apps/tauri-action` с `releaseDraft: true`, `includeUpdaterJson: true`,
`createUpdaterArtifacts: true`:
- собирает `jarvis` (фичи `wakeword-ort,whisper-native,stt-vad`),
- ad-hoc подписывает `.app` (без Apple/нотаризации), пакует `.dmg`,
- генерит `latest.json` (для авто-апдейтера) с подписью,
- создаёт draft-релиз «Jarvis vX.Y.Z» с ассетами.

## Авто-обновление в приложении

- Клиент проверяет `plugins.updater.endpoints` (latest.json) на старте; при свежей
  версии скачивает и ставит. Реализация — `src-tauri/src/main.rs` (updater).
- Чтобы апдейт долетел до пользователя, у него ДОЛЖНА стоять версия с верным
  endpoint+pubkey. Пользователи старых версий с placeholder-endpoint обновляются
  один раз вручную (скачать новый .dmg).
- После публикации релиза `latest/download/latest.json` начинает отдавать новую
  версию — клиенты подхватят на следующем запуске.

## Политика версий и совместимости

См. `docs/release/versioning-and-migration.md` (полная политика). Кратко:
- **Semver:** patch — фиксы; minor — фичи; major — ломающие изменения.
- **Данные пользователя в `~/.jarvis/`** (вне бандла) — апдейт их НЕ трогает.
  НИКОГДА не хранить пользовательские данные внутри `.app`.
- **`settings.json`:** загрузка мержит дефолты → добавление полей безопасно
  (всегда `#[serde(default)]`). Переименование/удаление/смена смысла поля —
  только через миграцию (`schemaVersion` + migrate-шаг в `settings.rs`).
- **Не ломать:** не менять смысл существующих полей; новое поведение — за
  дефолтным флагом настройки; парс-ошибки state/history → тихий скип (уже так).

## Чек-лист публикации

- [ ] Версия поднята в 4 файлах, PR смержен
- [ ] Тег запушен, `release.yml` — `success`
- [ ] Draft-релиз содержит `.dmg` и `latest.json`
- [ ] (если меняли схему настроек) добавлена миграция и поднят `schemaVersion`
- [ ] Опубликован: `gh release edit vX.Y.Z --draft=false`
- [ ] Проверено авто-обновление с предыдущей версии (по возможности)

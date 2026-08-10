# Jarvis UI Next

Браузерный React MVP нового интерфейса Jarvis. Он работает на моковых данных и
не подключён к production Tauri runtime.

## Запуск

Нужен Node.js 20.19+.

```bash
npm install
npm run dev -- --host 127.0.0.1 --port 8777
```

Открыть: <http://localhost:8777/>

Проверки:

```bash
npm run typecheck
npm run test
npm run build
```

## Маршруты

- `#/chats` — активные сессии;
- `#/chats/:sessionId` — чат, вопросы, task board и документы;
- `#/projects` — каталог проектов;
- `#/projects/:projectId/history` — история проекта;
- `#/projects/:projectId/workspace` — Agent VM workspace;
- `#/projects/environments` — все среды;
- `#/projects/environments/:vmId/files` — mounts и файлы;
- `#/stats` — usage и лимиты;
- `#/voice` — история, статистика, словарь, преобразования и черновик;
- `#/settings/:pane` — настройки, начиная с `appearance`.

## Архитектура MVP

`src/core/client/types.ts` содержит доменные типы. `JarvisClient` является
единственной границей действий UI, а `MockJarvisClient` реализует её для
браузера. Компоненты не вызывают Tauri API напрямую.

Zustand хранит snapshot, route и UI-состояния. Feature-компоненты подписываются
на необходимые selectors. Общие controls находятся в `src/shared/ui`, темы и
размеры — в `src/styles`.

При реальной миграции mock-клиент заменяется `TauriJarvisClient`; UI и сценарии
остаются теми же.

## Что намеренно замокано

- запуск, перезапуск и остановка VM;
- отправка сообщений и ответы на вопросы;
- установка моделей и проверки микрофона/backend;
- Finder, редактор, терминал, clipboard и внешние ссылки;
- repair конфигурации, onboarding и системные уведомления.

Каждое такое действие даёт видимое состояние или toast. Нативный side effect
подключается позже через typed client.

## Дизайн

Настройки сохраняют геометрию действующего `ui/settings2.js`: sidebar 248 px,
строки 14/12.5 px и отступы 15×16 px. Новая секция `Внешний вид` расположена
первой и содержит темы, краски и демонстрацию recoverable error.

Вспомогательный Figma-файл:
[Jarvis UI Next · React MVP](https://www.figma.com/design/i2mzds3ZsBwKatIsfGcmm7).

Полный функциональный baseline:
[`docs/superpowers/specs/2026-08-07-react-ui-mvp-functional-inventory.md`](../docs/superpowers/specs/2026-08-07-react-ui-mvp-functional-inventory.md).


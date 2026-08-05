# Jarvis под Linux

Порт macOS-приложения на Linux. Ядро — реестр сессий, панель, чат, уведомления,
настройки, голос — кросс-платформенное; расходятся только те места, где нужен
системный API: окна, питание, терминал, медиа.

## Сборка

Нужны WebKitGTK (движок webview у Tauri), GTK и appindicator (трей), ALSA
(микрофон и воспроизведение) и cmake (whisper.cpp).

```bash
# Debian / Ubuntu
sudo apt install libwebkit2gtk-4.1-dev libgtk-3-dev libayatana-appindicator3-dev \
                 librsvg2-dev libasound2-dev libxdo-dev patchelf cmake

# Fedora
sudo dnf install webkit2gtk4.1-devel gtk3-devel libappindicator-gtk3-devel \
                 librsvg2-devel alsa-lib-devel libxdo-devel patchelf cmake

# Arch
sudo pacman -S webkit2gtk-4.1 gtk3 libayatana-appindicator librsvg \
               alsa-lib xdotool patchelf cmake
```

```bash
npm run start:linux      # собрать и запустить
npm run bundle:linux     # .deb / .AppImage / .rpm в src-tauri/target/release/bundle
```

Wake-word (`wakeword-ort`) и VAD (`stt-vad`) тянут onnxruntime и в дефолтную
сборку не входят — как и на macOS.

## Полезные, но необязательные утилиты

Ничего из этого не является зависимостью: без них соответствующая функция просто
не срабатывает, а приложение работает дальше.

| Утилита | Зачем |
| --- | --- |
| `playerctl` | пауза чужой музыки на время озвучки (MPRIS) |
| `wmctrl` или `xdotool` | переход к терминалу сессии, список окон для «пока жив процесс» |
| `xdotool` (X11) / `wtype` (Wayland) | вставка результата диктовки в активное окно |
| `pactl` (PipeWire/Pulse) или `amixer` | управление системной громкостью голосом |
| `systemd-inhibit` | режим «не спать», пока работают агенты |
| эмулятор терминала | запуск сессии из вкладки «Проекты» |

## Что отличается от macOS

**Окна.** Накладка ⌘J держится поверх всего через штатный always-on-top; на
macOS для этого нужен уровень screen-saver. Показать окно, не забирая фокус,
на X11/Wayland нельзя — политику фокуса решает оконный менеджер. Панель
появляется на текущем/основном мониторе, а не на том, где курсор: глобальной
позиции курсора без обращения к серверу отображения нет.

**Нативного блюра подложки нет.** На macOS под панелью работает
NSVisualEffectView; WebKitGTK такого не умеет. Фон рисует сама бумажная
карточка — благодаря редизайну «Клевер» она непрозрачная, так что выглядит
одинаково. Скруглённым углам нужен композитор (он есть в GNOME, KDE, wlroots).

**Терминал.** Точный переход к вкладке по tty недоступен: ни один эмулятор не
отдаёт наружу соответствие tty→вкладка. Лесенка перехода становится короче на
одну ступень — сначала tmux (он кросс-платформенный и самый точный), потом
окно приложения-владельца через `wmctrl`/`xdotool`.

Запуск сессии ищет эмулятор сам: `x-terminal-emulator`, `gnome-terminal`,
`konsole`, `ptyxis`, `xfce4-terminal`, `kitty`, `alacritty`, `wezterm`, `foot`,
`tilix`, `terminator`, `mate-terminal`, `xterm`. Конкретный можно задать в
настройках «Запуск», а шаблон `custom` работает как и раньше.

**Диктовка.** Синтез Ctrl+V идёт через `xdotool` (X11) или `wtype` (Wayland).
На Wayland вставка сработает не везде: композитор может не поддерживать
протокол виртуальной клавиатуры. Текст в любом случае остаётся в буфере обмена,
и его можно вставить руками.

Снимка сфокусированного элемента (на macOS — Accessibility API) нет: аналог
AT-SPI отдаёт данные только у приложений с включённой доступностью, а у
терминалов и Electron это не работает. Проверка вставки идёт по факту.

**Крышка (closed-display mode) не поддерживается.** Это понятие ноутбуков Apple:
`pmset -a disablesleep` плюс sudoers-правило под него. На Linux поведением
крышки распоряжается logind (`HandleLidSwitch` в `/etc/systemd/logind.conf`) —
системная настройка, которую приложение менять не должно. Режим честно
отвечает «недоступен», и UI его прячет.

**Иконка в доке/панели задач.** `ActivationPolicy` — понятие AppKit. Место
приложения в панели задач решает оконный менеджер по флагу `skip_taskbar`,
который выставляется при создании окна: накладка скрыта, оконный режим виден.

**Автообновление.** Updater собирает артефакты и для Linux (AppImage), но
релизный workflow сейчас публикует только macOS-сборку — Linux ставится из
`.deb`/`.rpm`/AppImage вручную.

## Где что лежит

| Файл | Что разведено |
| --- | --- |
| `src-tauri/src/platform/` | окна, медиа, аудиовыход: `macos.rs` и `linux.rs` за общим API |
| `src-tauri/src/terminal.rs` | опознание и активация окна-владельца |
| `src-tauri/src/launch.rs` | запуск сессии в эмуляторе терминала |
| `src-tauri/src/power/assertion.rs` | IOPMAssertion ↔ `systemd-inhibit` |
| `src-tauri/src/power/clamshell_linux.rs` | заглушка closed-display |
| `src-tauri/src/stt/insert.rs` | синтез «вставить» |
| `src-tauri/src/convo/os.rs` | запуск приложений и громкость |
| `src-tauri/src/ipc.rs` | открыть файл / показать в папке / ссылка в браузер |

Правило простое: платформенное прячется за `mod imp` с `#[cfg]`, общий код
зовёт одну функцию и про ОС не знает.

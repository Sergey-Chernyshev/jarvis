//! Окна Jarvis: панель (raycast-стиль) и стек тостов.
//!
//! Оба окна создаются на старте скрытыми и живут весь срок демона:
//! закрытие панели (⌘W, крестик) — это hide, не destroy.

use serde::Serialize;
use serde_json::json;
use std::sync::Arc;
use tauri::utils::config::WindowEffectsConfig;
use tauri::window::{Effect, EffectState};
use tauri::{AppHandle, Emitter, Manager, Theme, WebviewUrl, WebviewWindow, WebviewWindowBuilder};

use crate::daemon::Daemon;
use crate::platform;

pub const PANEL_W: f64 = 820.0;
pub const PANEL_H: f64 = 620.0;
pub const TOAST_W: f64 = 440.0;
pub const TOAST_MAX_H: f64 = 480.0;
pub const ONBOARD_W: f64 = 480.0;
pub const ONBOARD_H: f64 = 600.0;
pub const AGENT_W: f64 = 460.0;
pub const AGENT_H: f64 = 600.0;
pub const AGENT_MIN_W: f64 = 360.0;
pub const AGENT_MIN_H: f64 = 380.0;

/// Оконный режим (макет 14h): список слева 264px + диалог справа.
pub const WINDOW_W: f64 = 1120.0;
pub const WINDOW_H: f64 = 640.0;
pub const WINDOW_MIN_W: f64 = 720.0;
pub const WINDOW_MIN_H: f64 = 420.0;

/// Настройка `mode`: `true` — обычное окно, `false` — накладка ⌘J.
/// try_state, а не Daemon::get: окна строятся до регистрации стейта.
pub fn is_window_mode(app: &AppHandle) -> bool {
    app.try_state::<Arc<Daemon>>()
        .map(|d| d.settings.string("mode") == "window")
        .unwrap_or(false)
}

/// Где в настройках живёт геометрия окна и какой она бывает по умолчанию.
/// Точка — на равных с размером: окно, растянутое на внешнем мониторе, обязано
/// открыться там же, а не в центре встроенного экрана.
struct Geom {
    w: &'static str,
    h: &'static str,
    x: &'static str,
    y: &'static str,
    min: (f64, f64),
    def: (f64, f64),
}

const GEOM_MAIN: Geom = Geom {
    w: "windowW",
    h: "windowH",
    x: "windowX",
    y: "windowY",
    min: (WINDOW_MIN_W, WINDOW_MIN_H),
    def: (WINDOW_W, WINDOW_H),
};
const GEOM_CHAT: Geom = Geom {
    w: "chatW",
    h: "chatH",
    x: "chatX",
    y: "chatY",
    min: (AGENT_MIN_W, AGENT_MIN_H),
    def: (AGENT_W, AGENT_H),
};

/// Запомненный размер окна (или размер из макета, если ещё не меняли).
fn saved_size(app: &AppHandle, g: &Geom) -> (f64, f64) {
    let Some(d) = app.try_state::<Arc<Daemon>>() else {
        return g.def;
    };
    let cfg = d.settings.load();
    let num = |k: &str, def: f64| cfg.get(k).and_then(|v| v.as_f64()).unwrap_or(def);
    (num(g.w, g.def.0).max(g.min.0), num(g.h, g.def.1).max(g.min.1))
}

/// Запомненная точка окна — только если она попадает хоть в один экран.
/// Внешний монитор могли отключить: честнее центр, чем окно за краем мира.
fn saved_pos(app: &AppHandle, g: &Geom) -> Option<(f64, f64)> {
    let d = app.try_state::<Arc<Daemon>>()?;
    let cfg = d.settings.load();
    let x = cfg.get(g.x)?.as_f64()?;
    let y = cfg.get(g.y)?.as_f64()?;
    on_any_screen(x, y, &screens(app)).then_some((x, y))
}

/// Экраны в логических координатах: (x, y, ширина, высота). Каждый переводим
/// его собственным масштабом — на смешанном DPI общий множитель врёт.
fn screens(app: &AppHandle) -> Vec<(f64, f64, f64, f64)> {
    app.available_monitors()
        .unwrap_or_default()
        .iter()
        .map(|m| {
            let sf = m.scale_factor();
            let p = m.position().to_logical::<f64>(sf);
            let s = m.size().to_logical::<f64>(sf);
            (p.x, p.y, s.width, s.height)
        })
        .collect()
}

/// Точка попадает хоть в один экран?
fn on_any_screen(x: f64, y: f64, screens: &[(f64, f64, f64, f64)]) -> bool {
    screens
        .iter()
        .any(|(sx, sy, sw, sh)| x >= *sx && x < sx + sw && y >= *sy && y < sy + sh)
}

/// Тема нативного материала окна. Панель — непрозрачная «бумага», но по
/// скруглённым углам просвечивает NSVisualEffectView: он должен совпадать с
/// выбранной темой, иначе на светлой панели видна тёмная кайма.
/// `auto` отдаём системе (`None` — Tauri берёт системную).
fn window_theme(app: &AppHandle) -> Option<Theme> {
    // окна строятся на старте — демон может быть ещё не зарегистрирован в state,
    // поэтому try_state, а не Daemon::get (тот паникует на отсутствующем стейте)
    let theme = app
        .try_state::<Arc<Daemon>>()
        .map(|d| d.settings.string("theme"))
        .unwrap_or_else(|| "light".into());
    match theme.as_str() {
        "dark" => Some(Theme::Dark),
        "auto" => None,
        _ => Some(Theme::Light),
    }
}

/// Главное окно. В накладке — раскладка Raycast (поверх всего, без дока,
/// не тянется); в оконном режиме — обычное окно: тянется, сворачивается,
/// живёт на своём уровне. Разметка одна и та же, её перестраивает CSS
/// по `data-mode` (см. `ui/theme.js`).
pub fn create_panel(app: &AppHandle) -> tauri::Result<WebviewWindow> {
    let window_mode = is_window_mode(app);
    let (w, h) = if window_mode {
        saved_size(app, &GEOM_MAIN)
    } else {
        (PANEL_W, PANEL_H)
    };
    let mut b = WebviewWindowBuilder::new(app, "main", WebviewUrl::App("index.html".into()))
        .title("Jarvis")
        .inner_size(w, h)
        .min_inner_size(WINDOW_MIN_W, WINDOW_MIN_H)
        .visible(false)
        // светофор рисуем сами (14h), поэтому системных декораций нет в обоих режимах
        .decorations(false)
        // настоящий блюр подложки: нативный NSVisualEffectView, не CSS
        .transparent(true)
        .effects(WindowEffectsConfig {
            effects: vec![Effect::UnderWindowBackground],
            state: Some(EffectState::Active), // блюр не гаснет у неактивного окна (тихий показ)
            radius: Some(16.0),
            color: None,
        })
        .resizable(window_mode)
        .minimizable(window_mode)
        .maximizable(window_mode)
        .skip_taskbar(!window_mode)
        .shadow(true)
        .theme(window_theme(app)) // материал под тему из настроек (см. window_theme)
        .accept_first_mouse(true);
    // накладку ставит place_panel на дисплей с курсором, а окно — туда, где его
    // оставили; без этого tao каждый раз центрирует его на главном дисплее
    if window_mode {
        if let Some((x, y)) = saved_pos(app, &GEOM_MAIN) {
            b = b.position(x, y);
        }
    }
    let win = b.build()?;
    if window_mode {
        platform::float_normal(&win);
    } else {
        platform::float_above_everything(&win);
    }
    Ok(win)
}

/// Переключение режима на лету: окно уже создано, поэтому меняем его свойства,
/// а не пересоздаём (иначе улетели бы открытый чат и позиция). Место в доке
/// меняется здесь же — см. ниже, почему это часть режима, а не косметика.
pub fn apply_mode(d: &Arc<Daemon>) {
    let Some(win) = d.app.get_webview_window("main") else {
        return;
    };
    let window_mode = d.settings.string("mode") == "window";
    // Место в доке — часть режима, а не косметика: без иконки окно нельзя
    // вернуть ни ⌘Tab, ни кликом, и оно выглядит «пропавшим». Раньше политика
    // ставилась только на старте, поэтому переключение режима на лету
    // оставляло окно без дока до перезапуска.
    #[cfg(target_os = "macos")]
    {
        let _ = d.app.set_activation_policy(if window_mode {
            tauri::ActivationPolicy::Regular
        } else {
            tauri::ActivationPolicy::Accessory
        });
    }
    let _ = win.set_resizable(window_mode);
    let _ = win.set_maximizable(window_mode);
    let _ = win.set_minimizable(window_mode);
    let _ = win.set_skip_taskbar(!window_mode);
    if window_mode {
        platform::float_normal(&win);
        let (w, h) = saved_size(&d.app, &GEOM_MAIN);
        let _ = win.set_size(tauri::LogicalSize::new(w, h));
        match saved_pos(&d.app, &GEOM_MAIN) {
            Some((x, y)) => {
                let _ = win.set_position(tauri::LogicalPosition::new(x, y));
            }
            None => {
                let _ = win.center();
            }
        }
        let _ = win.show();
        let _ = win.set_focus();
    } else {
        // из фуллскрина накладку не построишь — выходим до смены геометрии
        let _ = win.set_fullscreen(false);
        platform::float_above_everything(&win);
        let _ = win.set_size(tauri::LogicalSize::new(PANEL_W, PANEL_H));
        position_panel(d);
    }
}

/// Запомнить геометрию окна, чтобы следующий запуск открылся таким же — и там же.
/// Снимаем один раз (закрытие, выход), а не на каждом кадре ресайза.
pub fn remember_geometry(d: &Arc<Daemon>, label: &str) {
    let g = match label {
        // накладка не тянется и живёт под курсором — её геометрию считает place_panel
        "main" if d.settings.string("mode") == "window" => &GEOM_MAIN,
        "agent-chat" => &GEOM_CHAT,
        _ => return,
    };
    let Some(win) = d.app.get_webview_window(label) else {
        return;
    };
    let (Ok(size), Ok(sf)) = (win.inner_size(), win.scale_factor()) else {
        return;
    };
    let size = size.to_logical::<f64>(sf);
    let mut patch = serde_json::Map::new();
    patch.insert(g.w.into(), json!(size.width.round()));
    patch.insert(g.h.into(), json!(size.height.round()));
    if let Ok(pos) = win.outer_position() {
        let pos = pos.to_logical::<f64>(sf);
        patch.insert(g.x.into(), json!(pos.x.round()));
        patch.insert(g.y.into(), json!(pos.y.round()));
    }
    d.settings.save(patch);
}

/// Окно онбординга первого запуска (стеклянное, по центру). Повторный вызов из
/// меню — показать и сфокусировать существующее, а не плодить копии.
pub fn create_onboarding(app: &AppHandle) -> tauri::Result<WebviewWindow> {
    if let Some(win) = app.get_webview_window("onboarding") {
        let _ = win.show();
        let _ = win.set_focus();
        return Ok(win);
    }
    let win =
        WebviewWindowBuilder::new(app, "onboarding", WebviewUrl::App("onboarding.html".into()))
            .title("Jarvis")
            .inner_size(ONBOARD_W, ONBOARD_H)
            .visible(true)
            .decorations(false)
            .transparent(true)
            .effects(WindowEffectsConfig {
                effects: vec![Effect::UnderWindowBackground],
                state: Some(EffectState::Active),
                radius: Some(16.0),
                color: None,
            })
            .resizable(false)
            .minimizable(false)
            .maximizable(false)
            .skip_taskbar(true)
            .shadow(true)
            .center()
            .theme(window_theme(app))
            .accept_first_mouse(true)
            .build()?;
    let _ = win.set_focus();
    Ok(win)
}

/// Окно чата с агентом (фаза 7): стеклянное, ресайзится, помнит размер и место.
/// Повторный вызов — показать существующее, а не плодить копии: закрытие окна —
/// это hide (см. обработчик CloseRequested), поэтому нить разговора переживает
/// ⌘W вместе с геометрией.
pub fn create_agent_chat(app: &AppHandle) -> tauri::Result<WebviewWindow> {
    if let Some(win) = app.get_webview_window("agent-chat") {
        let _ = win.show();
        let _ = win.set_focus();
        return Ok(win);
    }
    let (w, h) = saved_size(app, &GEOM_CHAT);
    let mut b =
        WebviewWindowBuilder::new(app, "agent-chat", WebviewUrl::App("agent-chat.html".into()))
            .title("Jarvis · агент")
            .inner_size(w, h)
            .min_inner_size(AGENT_MIN_W, AGENT_MIN_H)
            .visible(true)
            .decorations(false)
            .transparent(true)
            .effects(WindowEffectsConfig {
                effects: vec![Effect::UnderWindowBackground],
                state: Some(EffectState::Active),
                radius: Some(16.0),
                color: None,
            })
            .resizable(true)
            .minimizable(false)
            .maximizable(false)
            .skip_taskbar(true)
            .shadow(true)
            .theme(window_theme(app))
            .accept_first_mouse(true);
    b = match saved_pos(app, &GEOM_CHAT) {
        Some((x, y)) => b.position(x, y),
        None => b.center(),
    };
    let win = b.build()?;
    let _ = win.set_focus();
    Ok(win)
}

/// Превью работы агента: отдельное окно с локальным адресом проекта.
///
/// Обычные декорации и никакого блюра — это чужая страница, и делать вид, что
/// она часть панели, нечестно: человек должен видеть, что смотрит своё
/// приложение, а не наш интерфейс.
///
/// Адрес разрешаем только локальный: окно панели живёт с её правами, и
/// открывать в нём произвольный сайт по строке из webview — не то, что стоит
/// уметь. Проверка — в `preview_url`.
pub fn create_preview(app: &AppHandle, url: &str) -> tauri::Result<WebviewWindow> {
    let parsed: tauri::Url = url.parse().map_err(|_| tauri::Error::WebviewNotFound)?;
    if let Some(win) = app.get_webview_window("preview") {
        let _ = win.close();
    }
    let win = WebviewWindowBuilder::new(app, "preview", WebviewUrl::External(parsed))
        .title(format!("Превью · {url}"))
        .inner_size(900.0, 700.0)
        .min_inner_size(320.0, 320.0)
        .visible(true)
        .resizable(true)
        .center()
        .theme(window_theme(app))
        .build()?;
    let _ = win.set_focus();
    Ok(win)
}

/// Адрес превью: только свой компьютер.
///
/// Превью существует, чтобы посмотреть, что подняла задача, — это всегда
/// localhost. Пускать сюда любой адрес значило бы дать webview открывать чужие
/// сайты в окне с правами панели.
pub fn preview_url(raw: &str) -> Result<String, String> {
    let raw = raw.trim();
    let with_scheme = if raw.starts_with("http://") || raw.starts_with("https://") {
        raw.to_string()
    } else {
        format!("http://{raw}")
    };
    let host = with_scheme
        .split("//")
        .nth(1)
        .and_then(|rest| rest.split('/').next())
        .map(|hostport| hostport.split(':').next().unwrap_or("").to_string())
        .unwrap_or_default();
    match host.as_str() {
        "localhost" | "127.0.0.1" | "0.0.0.0" | "[::1]" | "::1" => Ok(with_scheme),
        "" => Err("пустой адрес".into()),
        other => Err(format!(
            "превью открывает только адреса этого компьютера, а «{other}» — чужой"
        )),
    }
}

pub fn create_toast(app: &AppHandle) -> tauri::Result<WebviewWindow> {
    let win = WebviewWindowBuilder::new(app, "toast", WebviewUrl::App("toast.html".into()))
        // Заголовок нужен не человеку (декораций у окна нет), а оконному
        // менеджеру: на Wayland правила пишут по app_id и title, и безымянное
        // окно от панели не отличить. С ним правило Sway «тост не берёт фокус
        // и висит поверх» пишется одной строкой.
        .title("Jarvis · уведомление")
        .inner_size(TOAST_W, 120.0)
        .visible(false)
        .decorations(false)
        .transparent(true)
        .resizable(false)
        .minimizable(false)
        .maximizable(false)
        .skip_taskbar(true)
        .shadow(false) // форму рисует карточка, а не системное окно
        .focusable(false) // клики работают, фокус не воруется
        .accept_first_mouse(true)
        .theme(window_theme(app))
        .build()?;
    platform::float_above_everything(&win);
    Ok(win)
}

/* ================= доставка событий в окна ================= */

pub fn emit_to_panel<P: Serialize + Clone>(app: &AppHandle, event: &str, payload: &P) {
    let _ = app.emit_to("main", event, payload.clone());
}

/// Вид сменили — разослать всем окнам, чтобы панель, тосты, чат и онбординг
/// перестроились одновременно (`theme.js` слушает `appearance`).
/// Шлём снимок целиком: полей немного, а частичный патч заставил бы каждое
/// окно домысливать недостающее.
pub fn broadcast_appearance(d: &Arc<Daemon>) {
    let cfg = d.settings.load();
    let get = |k: &str| cfg.get(k).cloned().unwrap_or(serde_json::Value::Null);
    let payload = json!({
        "theme": get("theme"),
        "paint": get("paint"),
        "mode": get("mode"),
        "accent": get("accent"),
        "density": get("density"),
        "radius": get("radius"),
        "scale": get("scale"),
    });
    for label in ["main", "toast", "agent-chat", "onboarding"] {
        let _ = d.app.emit_to(label, "appearance", payload.clone());
    }
}

/// Эмит события напрямую в окно `toast` (для прямых эмиттеров вне `Daemon`,
/// напр. AudioHub — он держит только `AppHandle`, не буфер тостов).
pub fn emit_to_toast_window<P: Serialize + Clone>(app: &AppHandle, event: &str, payload: &P) {
    let _ = app.emit_to("toast", event, payload.clone());
}

/// Голос начал говорить эту карточку — держим открытой (не закрываем по TTL).
pub fn toast_hold(app: &AppHandle, id: &str) {
    let _ = app.emit_to("toast", "toast-hold", json!({ "id": id }));
}

/// Голос закончил — карточка живёт ещё `ms` (≈3.5с после речи).
pub fn toast_extend(app: &AppHandle, id: &str, ms: u64) {
    let _ = app.emit_to("toast", "toast-extend", json!({ "id": id, "ms": ms }));
}

/// Снять карточку тоста по id (вопрос ответили → убрать «липкую» карточку).
pub fn toast_remove(d: &Daemon, id: &str) {
    toast_emit(d, "toast-remove", json!({ "id": id }));
}

/// События тостов до загрузки webview буферятся (аналог did-finish-load
/// в Electron) — уведомления первых секунд после старта демона не теряются.
fn toast_emit(d: &Daemon, event: &'static str, payload: serde_json::Value) {
    if d.toast_ready.load(std::sync::atomic::Ordering::SeqCst) {
        let _ = d.app.emit_to("toast", event, payload);
    } else {
        d.pending_toasts.lock().unwrap().push((event, payload));
    }
}

/// Эмит голосового HUD-события (`voice-hud`) в окно `toast`. НАПРЯМУЮ (не через
/// буфер ранних тостов): фазы цикла — реалтайм, проигрывать «протухшую» фазу с
/// прошлого запуска бессмысленно; а буфер флашится по armed()=onAdd+onUpdate, и
/// voice-hud мог флашнуться ДО регистрации своего слушателя (F1).
pub fn hud_emit(d: &Daemon, payload: serde_json::Value) {
    let _ = d.app.emit_to("toast", "voice-hud", payload);
}

/// Мост тостов загрузился: доливаем накопленное в исходном порядке.
pub fn toast_flush(d: &Daemon) {
    d.toast_ready
        .store(true, std::sync::atomic::Ordering::SeqCst);
    for (event, payload) in d.pending_toasts.lock().unwrap().drain(..) {
        let _ = d.app.emit_to("toast", event, payload);
    }
}

pub fn toast_add(
    d: &Daemon,
    id: &str,
    title: &str,
    body: &str,
    session_id: Option<&str>,
    kind: &str,
    question: Option<&serde_json::Value>,
    meta: &serde_json::Value,
) {
    let payload = toast_payload(
        &d.settings.load(),
        id,
        title,
        body,
        session_id,
        kind,
        question,
        meta,
    );
    toast_emit(d, "toast-add", payload);
}

#[allow(clippy::too_many_arguments)]
fn toast_payload(
    settings: &serde_json::Value,
    id: &str,
    title: &str,
    body: &str,
    session_id: Option<&str>,
    kind: &str,
    question: Option<&serde_json::Value>,
    meta: &serde_json::Value,
) -> serde_json::Value {
    let ttl_ms = settings
        .pointer("/notify/ttlSec")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(8)
        .saturating_mul(1_000);

    json!({
        "id": id, "title": title, "body": body,
        "sessionId": session_id, "kind": kind, "question": question,
        "meta": meta, "ttlMs": ttl_ms,
    })
}

/* ================= позиционирование и показ панели ================= */

/// Панель — на дисплей с курсором (геометрия — в platform::place_panel:
/// AppKit-поинты, без конвертаций Tauri, иначе на смешанном DPI окно
/// уезжает на предыдущий экран).
pub fn position_panel(d: &Arc<Daemon>) {
    let Some(panel) = d.app.get_webview_window("main") else {
        return;
    };
    // окно пользователь ставит сам — не таскаем его под курсор на каждый показ
    if d.settings.string("mode") == "window" {
        return;
    }
    let corner = d.settings.string("position") == "corner";
    platform::place_panel(&panel, PANEL_W, PANEL_H, corner);
}

/// Показать панель по осознанному жесту человека — с фокусом.
///
/// Фокус здесь не про вежливость, а про то, чтобы панель ЗАКРЫВАЛАСЬ. Окно,
/// показанное через orderFrontRegardless, не становится key: до него не доходит
/// ни Esc, ни событие «фокус ушёл», по которому накладка прячется при клике
/// мимо. Такая панель висит поверх всего (включая чужой фуллскрин), и снять её
/// можно только вторым кликом по трею. Поэтому все входы, за которыми стоит
/// человек — клик по трею, пункт меню, хоткей, клик по тосту, `jarvis --show` —
/// зовут именно эту функцию.
pub fn show_panel(d: &Arc<Daemon>) {
    // пока интеграция не установлена — основное приложение «заперто»: ведём к онбордингу
    if !crate::install::integration_health().ok() {
        let _ = create_onboarding(&d.app);
        return;
    }
    let Some(panel) = d.app.get_webview_window("main") else {
        return;
    };
    position_panel(d);
    emit_to_panel(&d.app, "panel-shown", &json!(null));
    let _ = panel.show();
    let _ = panel.set_focus();
    d.push();
}

/// То же самое под именем, которое подчёркивает фокус в месте вызова
/// (хоткей, клик по тосту).
pub fn show_panel_focused(d: &Arc<Daemon>) {
    show_panel(d);
}

pub fn panel_visible(d: &Arc<Daemon>) -> bool {
    d.app
        .get_webview_window("main")
        .and_then(|w| w.is_visible().ok())
        .unwrap_or(false)
}

pub fn hide_panel(d: &Arc<Daemon>) {
    // запись сочетания не должна пережить панель — вернуть хоткеи
    crate::ipc::hotkeys_set_suspended(d, false);
    if let Some(panel) = d.app.get_webview_window("main") {
        let _ = panel.hide();
    }
}

/// Тумблер панели: клик по трею, ⌘J, `jarvis --toggle`.
pub fn toggle_panel(d: &Arc<Daemon>) {
    if panel_visible(d) && panel_in_sight(d) {
        hide_panel(d);
    } else {
        show_panel(d);
    }
}

/// Панель прямо перед глазами? «Видно» — ещё не значит «видно тебе»: обычное
/// окно стоит под чужими окнами, а накладка может висеть на другом мониторе.
/// В обоих случаях жест читается как «покажи», а не «спрячь», — прячем, только
/// когда человек смотрит на панель прямо сейчас.
fn panel_in_sight(d: &Arc<Daemon>) -> bool {
    if window_mode(d) {
        panel_focused(d)
    } else {
        panel_on_cursor_screen(d)
    }
}

/// Оконный режим по настройкам.
fn window_mode(d: &Arc<Daemon>) -> bool {
    d.settings.string("mode") == "window"
}

/// Окно сейчас в фокусе?
fn panel_focused(d: &Arc<Daemon>) -> bool {
    d.app
        .get_webview_window("main")
        .and_then(|w| w.is_focused().ok())
        .unwrap_or(false)
}

/// Панель на том же экране, где сейчас курсор (то есть где человек)?
/// Всё в физических координатах одного пространства — сравнение честное.
/// Не смогли выяснить — считаем, что на том же: это прежнее поведение.
fn panel_on_cursor_screen(d: &Arc<Daemon>) -> bool {
    let Some(win) = d.app.get_webview_window("main") else {
        return true;
    };
    let (Ok(cursor), Ok(pos), Ok(size)) =
        (d.app.cursor_position(), win.outer_position(), win.outer_size())
    else {
        return true;
    };
    let Ok(Some(mon)) = d.app.monitor_from_point(cursor.x, cursor.y) else {
        return true;
    };
    let (mp, ms) = (mon.position(), mon.size());
    let (cx, cy) = (
        pos.x as f64 + size.width as f64 / 2.0,
        pos.y as f64 + size.height as f64 / 2.0,
    );
    on_any_screen(
        cx,
        cy,
        &[(mp.x as f64, mp.y as f64, ms.width as f64, ms.height as f64)],
    )
}

/* ================= тост-окно ================= */

/// Рендерер тостов сообщает нужную высоту стека; 0 — спрятаться.
/// Низ прибит к краю экрана — окно растёт вверх.
pub fn toast_resize(d: &Arc<Daemon>, h: f64) {
    let Some(toast) = d.app.get_webview_window("toast") else {
        return;
    };
    if h <= 0.0 {
        let _ = toast.hide();
        return;
    }
    let height = h.round().clamp(1.0, TOAST_MAX_H);
    platform::place_toast(&toast, TOAST_W, height);
    if !toast.is_visible().unwrap_or(false) {
        platform::show_inactive(&toast);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Превью — про своё приложение. Чужой адрес из webview открывать нельзя,
    /// и отказ обязан объяснить, почему.
    #[test]
    fn preview_takes_only_local_addresses() {
        assert_eq!(preview_url("localhost:3000").unwrap(), "http://localhost:3000");
        assert_eq!(preview_url("http://127.0.0.1:8080/app").unwrap(), "http://127.0.0.1:8080/app");
        let e = preview_url("https://example.com").unwrap_err();
        assert!(e.contains("example.com"), "{e}");
        assert!(preview_url("").is_err());
    }

    /// Сохранённую точку применяем, только если она попадает хоть в один экран:
    /// внешний монитор отключили — окно должно вернуться в центр, а не уехать
    /// за край мира.
    #[test]
    fn saved_point_counts_only_inside_some_screen() {
        let builtin = (0.0, 0.0, 1440.0, 900.0);
        let external = (-1920.0, -300.0, 1920.0, 1080.0);

        assert!(on_any_screen(100.0, 100.0, &[builtin, external]));
        assert!(on_any_screen(-1800.0, -200.0, &[builtin, external]));
        // тот же внешний монитор, но отключённый — точки больше нет ни на одном
        assert!(!on_any_screen(-1800.0, -200.0, &[builtin]));
        // ровно на правой границе экрана — уже за ним
        assert!(!on_any_screen(1440.0, 10.0, &[builtin]));
        assert!(!on_any_screen(10.0, 10.0, &[]));
    }

    #[test]
    fn configured_toast_ttl_reaches_payload_in_milliseconds() {
        for (seconds, milliseconds) in [(5, 5_000), (8, 8_000), (0, 0)] {
            let settings = json!({ "notify": { "ttlSec": seconds } });
            let payload = toast_payload(
                &settings,
                "id",
                "title",
                "body",
                None,
                "done",
                None,
                &json!([]),
            );

            assert_eq!(payload["ttlMs"], milliseconds);
        }
    }

    #[test]
    fn invalid_toast_ttl_falls_back_to_eight_seconds() {
        for settings in [
            json!({}),
            json!({ "notify": { "ttlSec": "5" } }),
            json!({ "notify": { "ttlSec": -1 } }),
        ] {
            let payload = toast_payload(
                &settings,
                "id",
                "title",
                "body",
                None,
                "done",
                None,
                &json!([]),
            );

            assert_eq!(payload["ttlMs"], 8_000);
        }
    }
}

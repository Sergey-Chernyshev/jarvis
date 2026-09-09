//! «Не спать» на плагинном контракте — первая наша способность, переехавшая
//! в плагин (спека `2026-08-19-everything-is-plugin-design.md`, инкремент 4).
//!
//! Это **адаптер, а не переписывание**: движок, ассерты и связка с «Крышкой»
//! остались в `power/`; сюда переехало то, что раньше было хардкодом ядра —
//! манифест (тумблер и настройки), секция трея и поверхность команд.

use std::sync::Arc;

use serde_json::{json, Value};

use super::to_result;
use crate::daemon::Daemon;
use crate::plugin::contract::{CallFut, Plugin, StartFail, TrayItem};
use crate::plugin::manifest::Manifest;
use crate::power::{keep_awake, Power};

pub struct KeepAwake {
    manifest: Manifest,
}

impl KeepAwake {
    pub fn new() -> Self {
        let m = json!({
            "id": "keep-awake",
            "name": "Не спать",
            "version": "1.0.0",
            "description": "Не даёт машине уснуть, пока агенты работают",
            "icon": "coffee",
            "kind": "builtin",
            "defaultEnabled": true,
            "tray": true,
            // настройки живут в своей вкладке «Бодрость», а не в общем списке
            "pane": "awake",
            "settings": [
                { "key": "auto", "type": "toggle", "default": false,
                  "title": "Пока агенты работают",
                  "hint": "Держать машину бодрой, пока хоть одна сессия в работе." },
                { "key": "keepDisplayOn", "type": "toggle", "default": false,
                  "title": "Не гасить экран",
                  "hint": "Иначе экран гаснет, а машина не спит." }
            ],
            "commands": [
                { "name": "start-manual", "title": "Бессрочно" },
                { "name": "start-timer" },
                { "name": "start-process" },
                { "name": "stop", "title": "Выключить ручной режим" },
                { "name": "off" },
                { "name": "set" }
            ]
        });
        KeepAwake { manifest: Manifest::parse(m).expect("свой манифест обязан быть валидным") }
    }
}

impl Plugin<Arc<Daemon>> for KeepAwake {
    fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    fn start(&self, d: &Arc<Daemon>, _token: Option<&str>) -> Result<(), StartFail> {
        if !d.power.ka_enabled() {
            Power::activate_keep_awake(d);
        }
        Ok(())
    }

    fn stop(&self, d: &Arc<Daemon>) {
        Power::deactivate_keep_awake(d);
    }

    fn status(&self, d: &Arc<Daemon>) -> Value {
        d.power.ka_status(d).unwrap_or(Value::Null)
    }

    fn tray(&self, d: &Arc<Daemon>) -> Vec<TrayItem> {
        let Some(st) = d.power.ka_state() else { return Vec::new() };
        let s = Power::ka_settings(d);
        let line = keep_awake::status_line(&st, crate::util::now_ms());
        let mut out = vec![
            TrayItem::Label {
                text: match line {
                    Some(l) => format!("☕ Не спать: {l}"),
                    None => "☕ Не спать: выкл".into(),
                },
            },
            TrayItem::action("start-manual", "Бессрочно"),
            TrayItem::Submenu {
                text: "На время".into(),
                items: keep_awake::PRESETS_MIN
                    .iter()
                    .map(|m| TrayItem::Action {
                        id: format!("timer-{m}"),
                        text: keep_awake::preset_label(*m),
                        cmd: "start-timer".into(),
                        args: json!({ "minutes": m }),
                    })
                    .collect(),
            },
        ];
        let procs = d.power.processes_snapshot();
        out.push(TrayItem::Submenu {
            text: "Пока жив процесс".into(),
            items: if procs.is_empty() {
                vec![TrayItem::Label { text: "процессы не нашлись".into() }]
            } else {
                procs
                    .iter()
                    .take(24)
                    .map(|(pid, label)| TrayItem::Action {
                        id: format!("proc-{pid}"),
                        text: label.clone(),
                        cmd: "start-process".into(),
                        args: json!({ "pid": pid, "label": label }),
                    })
                    .collect()
            },
        });
        if !st["manual"].is_null() {
            out.push(TrayItem::action("stop", "Выключить ручной режим"));
        }
        out.push(TrayItem::Separator);
        // Пункт-галка сам несёт то, что нужно позвать: ядру не приходится
        // переводить «клик по set-auto» в «set {auto: !текущее}».
        let auto = s["auto"].as_bool().unwrap_or(false);
        out.push(TrayItem::Check {
            id: "set-auto".into(),
            text: "Пока агенты работают (авто)".into(),
            checked: auto,
            enabled: true,
            cmd: "set".into(),
            args: json!({ "auto": !auto }),
        });
        let display = s["keepDisplayOn"].as_bool().unwrap_or(false);
        out.push(TrayItem::Check {
            id: "set-display".into(),
            text: "Не гасить экран".into(),
            checked: display,
            enabled: true,
            cmd: "set".into(),
            args: json!({ "keepDisplayOn": !display }),
        });
        out
    }

    fn call(&self, d: Arc<Daemon>, name: String, args: Value) -> CallFut {
        Box::pin(async move { to_result(Power::ka_cmd(&d, &name, &args)) })
    }
}

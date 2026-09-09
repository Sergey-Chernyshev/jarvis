//! «Крышка» на плагинном контракте (инкремент 4 спеки «всё есть плагин»).
//!
//! Адаптер над `power::clamshell`: сама логика closed-display mode осталась
//! там (на Linux — честная заглушка), сюда переехали манифест, трей и команды.

use std::sync::Arc;

use serde_json::{json, Value};

use super::to_result;
use crate::daemon::Daemon;
use crate::plugin::contract::{CallFut, Plugin, StartFail, TrayItem};
use crate::plugin::manifest::Manifest;
use crate::power::{clamshell, Power};

pub struct Clamshell {
    manifest: Manifest,
}

impl Clamshell {
    pub fn new() -> Self {
        let m = json!({
            "id": "clamshell",
            "name": "Крышка",
            "version": "1.0.0",
            "description": "Держит машину бодрой даже с закрытой крышкой",
            "icon": "laptop",
            "kind": "builtin",
            "defaultEnabled": true,
            "tray": true,
            "pane": "awake",
            "settings": [
                { "key": "autoArm", "type": "toggle", "default": false,
                  "title": "Авто при работе агентов",
                  "hint": "Взводить режим, пока агенты работают. Нужен тихий режим (sudoers)." },
                { "key": "suggest", "type": "toggle", "default": true,
                  "title": "Подсказывать после прерванного сна" },
                { "key": "batteryFloor", "type": "number", "default": 15,
                  "min": 5, "max": 80,
                  "title": "Нижний порог батареи, %",
                  "hint": "Ниже — режим не взводится: закрытый ноут разрядится в ноль." }
            ],
            "commands": [
                { "name": "arm", "title": "Взвести" },
                { "name": "disarm", "title": "Снять" },
                { "name": "install-sudoers", "title": "Настроить тихий режим…" },
                { "name": "set" }
            ]
        });
        Clamshell { manifest: Manifest::parse(m).expect("свой манифест обязан быть валидным") }
    }
}

impl Plugin<Arc<Daemon>> for Clamshell {
    fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    fn start(&self, d: &Arc<Daemon>, _token: Option<&str>) -> Result<(), StartFail> {
        Power::activate_clamshell(d);
        Ok(())
    }

    fn stop(&self, d: &Arc<Daemon>) {
        Power::deactivate_clamshell(d);
    }

    fn status(&self, d: &Arc<Daemon>) -> Value {
        d.power.cs_status(d).unwrap_or(Value::Null)
    }

    fn tray(&self, d: &Arc<Daemon>) -> Vec<TrayItem> {
        let (active, armed, lid_causes_sleep) = d.power.clam_view();
        if !active {
            return Vec::new();
        }
        let s = Power::cs_settings(d);
        let sudoers = clamshell::sudoers_installed();
        let mut out = vec![
            TrayItem::Label {
                text: if armed {
                    "⌒ Крышка: мак не уснёт даже закрытой".into()
                } else if lid_causes_sleep == Some(false) {
                    "⌒ Крышка: закрытие сейчас не усыпляет".into()
                } else {
                    "⌒ Крышка: закроешь — уснёт".into()
                },
            },
            TrayItem::Check {
                id: "toggle".into(),
                text: "Closed-display mode".into(),
                checked: armed,
                enabled: true,
                // взведено → снять, снято → взвести: решает плагин, не ядро
                cmd: if armed { "disarm".into() } else { "arm".into() },
                args: json!({}),
            },
        ];
        let auto_arm = s["autoArm"].as_bool().unwrap_or(false);
        out.push(TrayItem::Check {
            id: "set-autoarm".into(),
            text: if sudoers {
                "Авто при работе агентов".into()
            } else {
                "Авто при работе агентов (нужен тихий режим)".into()
            },
            checked: auto_arm,
            enabled: sudoers,
            cmd: "set".into(),
            args: json!({ "autoArm": !auto_arm }),
        });
        let suggest = s["suggest"].as_bool().unwrap_or(false);
        out.push(TrayItem::Check {
            id: "set-suggest".into(),
            text: "Подсказывать после прерванного сна".into(),
            checked: suggest,
            enabled: true,
            cmd: "set".into(),
            args: json!({ "suggest": !suggest }),
        });
        if !sudoers {
            out.push(TrayItem::action("install-sudoers", "Настроить тихий режим (sudoers)…"));
        }
        out
    }

    fn call(&self, d: Arc<Daemon>, name: String, args: Value) -> CallFut {
        Box::pin(async move { to_result(Power::cs_cmd(&d, &name, &args).await) })
    }
}

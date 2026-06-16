//! Базовые типы слоя капабилити: класс риска, провенанс, метаданные, ошибки гейта.
//!
//! Это «анатомия одной капабилити» из спеки (§4): идентификатор, класс риска
//! (политика гейта), провенанс выхода (trusted/untrusted), описание для агента
//! и JSON-схема входа (проекция в MCP tool def).

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Класс риска — определяет политику гейта (§6).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RiskClass {
    /// Чтение. Агент вызывает автоматически.
    Read,
    /// Действие в сессии. Side-effect → подтверждение.
    Control,
    /// Изменение конфига. Side-effect → подтверждение.
    Settings,
    /// Администрирование платформы. Агенту/плагину не выдаётся никогда.
    Admin,
}

impl RiskClass {
    /// Side-effect — требует подтверждения по политике гранта.
    pub fn is_side_effect(self) -> bool {
        matches!(self, RiskClass::Control | RiskClass::Settings)
    }
    pub fn as_str(self) -> &'static str {
        match self {
            RiskClass::Read => "read",
            RiskClass::Control => "control",
            RiskClass::Settings => "settings",
            RiskClass::Admin => "admin",
        }
    }
}

/// Провенанс выхода капабилити (§8, слой a). Untrusted = вывод чужого
/// процесса/контента, потенциальный носитель инъекции (содержимое чатов).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Provenance {
    Trusted,
    Untrusted,
}

impl Provenance {
    pub fn as_str(self) -> &'static str {
        match self {
            Provenance::Trusted => "trusted",
            Provenance::Untrusted => "untrusted",
        }
    }
}

/// Декларация одной капабилити. `id`/`description` статичны (заданы в коде),
/// `input_schema` — JSON Schema для проекции в MCP tool def.
#[derive(Clone, Debug)]
pub struct CapabilityMeta {
    pub id: &'static str,
    pub class: RiskClass,
    pub provenance: Provenance,
    pub description: &'static str,
    pub input_schema: Value,
}

/// Результат вызова капабилити: значение + его провенанс (метка идёт в аудит
/// и, позже, для taint-распространения).
#[derive(Clone, Debug, Serialize)]
pub struct CallOutput {
    pub value: Value,
    pub provenance: Provenance,
}

/// Отказы гейта. Все — структурные, без живой LLM (приёмочный сценарий 4).
#[derive(Clone, Debug, PartialEq)]
pub enum GateError {
    /// Нет такой капабилити в реестре.
    NotFound(String),
    /// Грант не разрешает класс / защищённый ключ (самоэскалация).
    Denied(String),
    /// Пользователь отклонил подтверждение side-effect.
    Rejected,
    /// Хендлер капабилити вернул ошибку (сбой сервиса/поставщика).
    Failed(String),
}

impl GateError {
    /// Стабильный код для аудита/ответа MCP-серверу.
    pub fn code(&self) -> &'static str {
        match self {
            GateError::NotFound(_) => "not_found",
            GateError::Denied(_) => "denied",
            GateError::Rejected => "rejected",
            GateError::Failed(_) => "failed",
        }
    }
}

impl std::fmt::Display for GateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GateError::NotFound(id) => write!(f, "капабилити не найдена: {id}"),
            GateError::Denied(why) => write!(f, "отказано: {why}"),
            GateError::Rejected => write!(f, "пользователь отклонил подтверждение"),
            GateError::Failed(e) => write!(f, "сбой исполнения: {e}"),
        }
    }
}

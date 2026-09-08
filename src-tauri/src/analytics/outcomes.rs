//! User-reviewed task outcomes. Money is never inferred from token prices.
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

const MAX_OUTCOME_BYTES: usize = 8 * 1024 * 1024;
const MAX_OUTCOMES: usize = 10_000;
static WRITER: Mutex<()> = Mutex::new(());
static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

struct LimitedBytes(Vec<u8>);
impl Write for LimitedBytes {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        // Reserve one byte for the final newline. Never allocate the full
        // oversized serialization merely to discover it exceeds the limit.
        if bytes.len() > (MAX_OUTCOME_BYTES - 1).saturating_sub(self.0.len()) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "Файл результатов превышает 8 МиБ",
            ));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Outcome {
    #[serde(default)]
    pub id: String,
    pub session_id: String,
    #[serde(default)]
    pub project: String,
    pub task_type: String,
    pub model: String,
    pub harness_version: String,
    pub outcome: String,
    #[serde(default)]
    pub baseline_minutes: Option<f64>,
    #[serde(default)]
    pub ai_minutes: Option<f64>,
    #[serde(default)]
    pub review_minutes: Option<f64>,
    #[serde(default)]
    pub rework_minutes: Option<f64>,
    #[serde(default)]
    pub hourly_rate: Option<f64>,
    #[serde(default)]
    pub actual_cost: Option<f64>,
    pub currency: String,
    #[serde(default)]
    pub baseline_source: Option<String>,
    #[serde(default)]
    pub reviewer_evidence: String,
    #[serde(default)]
    pub recorded_at: i64,
}

impl Outcome {
    fn validate(&self) -> Result<(), String> {
        for (name, text) in [
            ("sessionId", &self.session_id),
            ("taskType", &self.task_type),
            ("model", &self.model),
            ("harnessVersion", &self.harness_version),
        ] {
            if text.trim().is_empty() || text.len() > 240 {
                return Err(format!("{name}: требуется значение длиной до 240 байт"));
            }
        }
        if self.id.len() > 240 || self.project.len() > 4096 || self.reviewer_evidence.len() > 4000 {
            return Err("Слишком длинное поле результата".into());
        }
        if !matches!(self.outcome.as_str(), "accepted" | "rework" | "rejected") {
            return Err("outcome: accepted, rework или rejected".into());
        }
        if self.currency.len() != 3 || !self.currency.bytes().all(|b| b.is_ascii_uppercase()) {
            return Err(
                "currency: код из трёх заглавных латинских букв, например USD, RUB, EUR, GBP, CNY"
                    .into(),
            );
        }
        for number in [
            self.baseline_minutes,
            self.ai_minutes,
            self.review_minutes,
            self.rework_minutes,
            self.hourly_rate,
            self.actual_cost,
        ]
        .into_iter()
        .flatten()
        {
            if !number.is_finite() || !(0.0..=100_000_000.0).contains(&number) {
                return Err("Числа должны быть конечными и неотрицательными".into());
            }
        }
        if self
            .baseline_source
            .as_deref()
            .is_some_and(|s| !matches!(s, "estimate" | "measured"))
            || (self.baseline_minutes.is_some() && self.baseline_source.is_none())
        {
            return Err("Для базы сравнения укажите baselineSource: estimate или measured".into());
        }
        Ok(())
    }
    fn minutes(&self) -> Option<f64> {
        Some(self.ai_minutes? + self.review_minutes? + self.rework_minutes?)
    }
    fn net(&self) -> Option<f64> {
        // A rejected/rework task has costs but no delivered time-saving benefit.
        let baseline = if self.outcome == "accepted" {
            self.baseline_minutes?
        } else {
            0.0
        };
        Some((baseline - self.minutes()?) * self.hourly_rate? / 60.0 - self.actual_cost?)
    }
}

pub fn load(dir: &Path) -> Result<Vec<Outcome>, String> {
    let path = dir.join("analytics-outcomes.json");
    let file = match fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(&path)
    {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
        Err(e) => return Err(e.to_string()),
        Ok(file) => file,
    };
    let metadata = file.metadata().map_err(|e| e.to_string())?;
    if !metadata.is_file() {
        return Err("Файл результатов не является обычным файлом".into());
    }
    if metadata.len() > MAX_OUTCOME_BYTES as u64 {
        return Err("Файл результатов превышает 8 МиБ".into());
    }
    let mut bytes = Vec::new();
    file.take((MAX_OUTCOME_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > MAX_OUTCOME_BYTES {
        return Err("Файл результатов превышает 8 МиБ".into());
    }
    let items: Vec<Outcome> =
        serde_json::from_slice(&bytes).map_err(|e| format!("Повреждён файл результатов: {e}"))?;
    if items.len() > MAX_OUTCOMES {
        return Err("Достигнут предел 10 000 результатов".into());
    }
    for item in &items {
        item.validate()?;
    }
    Ok(items)
}

pub fn save(dir: &Path, value: Value) -> Result<Value, String> {
    let mut outcome: Outcome = serde_json::from_value(value).map_err(|e| e.to_string())?;
    outcome.validate()?;
    let _lock = WRITER
        .lock()
        .map_err(|_| "Хранилище результатов недоступно")?;
    let mut items = load(dir)?;
    // Re-saving a form updates the same task rather than multiplying the profit.
    let existing = if outcome.id.is_empty() {
        items
            .iter()
            .position(|x| x.session_id == outcome.session_id && x.task_type == outcome.task_type)
    } else {
        Some(
            items
                .iter()
                .position(|x| x.id == outcome.id)
                .ok_or("Результат для обновления не найден")?,
        )
    };
    outcome.recorded_at = crate::util::now_ms();
    if let Some(index) = existing {
        outcome.id = items[index].id.clone();
        items[index] = outcome.clone();
    } else {
        if items.len() >= MAX_OUTCOMES {
            return Err("Достигнут предел 10 000 результатов".into());
        }
        outcome.id = format!("task-{}-{}", outcome.recorded_at, items.len());
        items.push(outcome.clone());
    }
    let mut serialized = LimitedBytes(Vec::new());
    serde_json::to_writer_pretty(&mut serialized, &items).map_err(|e| e.to_string())?;
    serialized.0.push(b'\n');
    fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let temporary = dir.join(format!(
        ".analytics-outcomes-{}-{}.tmp",
        std::process::id(),
        NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
    ));
    // An existing temporary path is never overwritten or removed: cleanup is
    // permitted only after this writer successfully created its own file.
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&temporary)
        .map_err(|e| e.to_string())?;
    let result = (|| {
        file.write_all(&serialized.0)
            .and_then(|_| file.sync_all())
            .map_err(|e| e.to_string())?;
        drop(file);
        fs::rename(&temporary, dir.join("analytics-outcomes.json")).map_err(|e| e.to_string())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result?;
    Ok(json!({"ok":true,"outcome":outcome}))
}

pub fn economics(items: &[Outcome]) -> Value {
    economics_with_config(items, &Value::Null)
}

pub fn economics_with_config(items: &[Outcome], config: &Value) -> Value {
    let min_samples = config
        .pointer("/rules/minModelSamples")
        .and_then(Value::as_u64)
        .unwrap_or(5)
        .clamp(1, 1000) as usize;
    let mut currencies: BTreeMap<String, Vec<&Outcome>> = BTreeMap::new();
    let mut spending: BTreeMap<String, (f64, usize, usize)> = BTreeMap::new();
    let mut cohorts: BTreeMap<(String, String, String, String, String), Vec<&Outcome>> =
        BTreeMap::new();
    for item in items {
        let spend = spending.entry(item.currency.clone()).or_default();
        spend.2 += 1;
        if let Some(cost) = item.actual_cost {
            spend.0 += cost;
            spend.1 += 1;
        }
        if item.net().is_some() {
            currencies
                .entry(item.currency.clone())
                .or_default()
                .push(item);
        }
        cohorts
            .entry((
                item.project.clone(),
                item.task_type.clone(),
                item.model.clone(),
                item.harness_version.clone(),
                item.currency.clone(),
            ))
            .or_default()
            .push(item);
    }
    let groups: Vec<Value> = currencies.into_iter().map(|(currency, rows)| {
        let mut base = 0.0;
        let mut minutes = 0.0;
        let mut cost = 0.0;
        let mut labor = 0.0;
        let mut net = 0.0;
        for row in &rows {
            base += if row.outcome == "accepted" { row.baseline_minutes.unwrap_or(0.0) } else { 0.0 };
            minutes += row.minutes().unwrap();
            cost += row.actual_cost.unwrap();
            labor += row.minutes().unwrap() * row.hourly_rate.unwrap() / 60.0;
            net += row.net().unwrap();
        }
        json!({"currency":currency,"taskCount":rows.len(),"netValue":net,"savedMinutes":base-minutes,
            "excludedIncompleteTasks":items.iter().filter(|r|r.currency==currency && r.net().is_none()).count(),
            "totalCost":cost,"laborCost":labor,"speedup":(minutes>0.0).then_some(base/minutes),
            "roiPct":((cost+labor)>0.0).then_some(100.0*net/(cost+labor)),
            "measuredBaselines":rows.iter().filter(|r|r.baseline_source.as_deref()==Some("measured")).count(),
            "basis":"user-recorded-baselines","roiBasis":"net-value-divided-by-ai-cost-plus-human-labor-cost"})
    }).collect();
    let mut comparisons: Vec<Value> = cohorts.into_iter().map(|((project,task,model,harness,currency),rows)| {
        let accepted = rows.iter().filter(|r|r.outcome=="accepted").count();
        let times: Option<Vec<f64>> = rows.iter().map(|r|r.minutes()).collect();
        let costs: Option<Vec<f64>> = rows.iter().map(|r|r.actual_cost).collect();
        let mean_time = times.map(|v|v.iter().sum::<f64>()/rows.len() as f64);
        let cost_success = costs.and_then(|v|(accepted>0).then_some(v.iter().sum::<f64>()/accepted as f64));
        let sufficient = rows.len() >= min_samples && mean_time.is_some() && cost_success.is_some();
        json!({"project":project,"taskType":task,"model":model,"harnessVersion":harness,"currency":currency,
            "samples":rows.len(),"minSamples":min_samples,"accepted":accepted,"acceptancePct":100.0*accepted as f64/rows.len() as f64,
            "meanTotalMinutes":mean_time,"costPerAccepted":cost_success,
            "status":if sufficient {"observational"} else {"insufficient-evidence"},
            "recommendation":if sufficient {"Сравните одинаковые задачи по приёмке, времени и цене; различие не доказывает влияние модели.".to_string()}
                else {format!("Недостаточно сопоставимых результатов с временем и затратами. Требуется хотя бы {min_samples} задач на вариант модели и харнеса (порог из настроек).")}})
    }).collect();
    // An observed Pareto candidate is a hypothesis for a paired evaluation,
    // never an automatic router or a causal claim about different task mixes.
    let snapshot = comparisons.clone();
    for row in &mut comparisons {
        if row["status"] != "observational" {
            continue;
        }
        let candidates: Vec<String> = snapshot
            .iter()
            .filter(|other| {
                other["status"] == "observational"
                    && other["model"] != row["model"]
                    && ["project", "taskType", "harnessVersion", "currency"]
                        .iter()
                        .all(|key| other[*key] == row[*key])
                    && other["acceptancePct"].as_f64() >= row["acceptancePct"].as_f64()
                    && other["meanTotalMinutes"].as_f64() <= row["meanTotalMinutes"].as_f64()
                    && other["costPerAccepted"].as_f64() <= row["costPerAccepted"].as_f64()
                    && (other["acceptancePct"].as_f64() > row["acceptancePct"].as_f64()
                        || other["meanTotalMinutes"].as_f64() < row["meanTotalMinutes"].as_f64()
                        || other["costPerAccepted"].as_f64() < row["costPerAccepted"].as_f64())
            })
            .filter_map(|other| other["model"].as_str().map(String::from))
            .collect();
        if !candidates.is_empty() {
            row["recommendation"]=json!(format!("Кандидаты для парного сравнения: {}. В записанной выборке у них не хуже приёмка, время и стоимость. Различия сложности задач и малые выборки могут объяснять результат; автоматическая смена модели не выполняется.",candidates.join(", ")));
        }
        row["candidateAlternatives"] = json!(candidates);
    }
    let spending: Vec<Value> = spending.into_iter().map(|(currency,(cost,known,total))|
        json!({"currency":currency,"knownActualCost":cost,"costRecordedTasks":known,"tasks":total,"missingCostTasks":total-known})).collect();
    json!({"recorded":items.len(),"complete":items.iter().filter(|r|r.net().is_some()).count(),"spending":spending,
        "accepted":items.iter().filter(|r|r.outcome=="accepted").count(),"groups":groups,"comparisons":comparisons,
        "caveat":"Стоимость — введённые фактические затраты. База сравнения и рабочее время указаны пользователем; это расчёт, не причинный эксперимент."})
}

#[cfg(test)]
mod tests {
    use super::*;
    fn row() -> Outcome {
        serde_json::from_value(json!({"sessionId":"claude:1","taskType":"fix","model":"m",
        "harnessVersion":"v1","outcome":"accepted","currency":"USD","baselineSource":"measured",
        "baselineMinutes":120,"aiMinutes":30,"reviewMinutes":15,"reworkMinutes":15,"hourlyRate":60,"actualCost":10})).unwrap()
    }
    #[test]
    fn profit_includes_review_and_rework() {
        let r = economics(&[row()]);
        assert_eq!(r["groups"][0]["netValue"], 50.0);
        assert_eq!(r["groups"][0]["speedup"], 2.0);
    }
    #[test]
    fn unknown_cost_not_zero_and_rejection_is_loss() {
        let mut r = row();
        r.actual_cost = None;
        assert!(economics(&[r.clone()])["groups"]
            .as_array()
            .unwrap()
            .is_empty());
        r.actual_cost = Some(10.0);
        r.outcome = "rejected".into();
        assert_eq!(r.net(), Some(-70.0));
    }
    #[test]
    fn currencies_and_harnesses_are_not_pooled() {
        let a = row();
        let mut b = row();
        b.currency = "RUB".into();
        b.harness_version = "v2".into();
        let r = economics(&[a, b]);
        assert_eq!(r["groups"].as_array().unwrap().len(), 2);
        assert_eq!(r["comparisons"].as_array().unwrap().len(), 2);
    }
    #[test]
    fn incomplete_tasks_keep_known_spending_and_pareto_is_cohort_scoped() {
        let mut incomplete = row();
        incomplete.ai_minutes = None;
        assert_eq!(
            economics(&[incomplete])["spending"][0]["knownActualCost"],
            10.0
        );
        let mut rows = vec![row(); 5];
        let mut cheaper = row();
        cheaper.model = "cheaper".into();
        cheaper.actual_cost = Some(5.0);
        rows.extend(vec![cheaper; 5]);
        let report = economics(&rows);
        let original = report["comparisons"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["model"] == "m")
            .unwrap();
        assert_eq!(original["candidateAlternatives"], json!(["cheaper"]));
    }

    #[test]
    fn custom_currency_and_minimum_sample_rule_are_supported() {
        let mut value = row();
        value.currency = "JPY".into();
        assert!(value.validate().is_ok());
        let rows = vec![value.clone(); 2];
        assert_eq!(
            economics(&rows)["comparisons"][0]["status"],
            "insufficient-evidence"
        );
        let report = economics_with_config(&rows, &json!({"rules":{"minModelSamples":2}}));
        assert_eq!(report["comparisons"][0]["status"], "observational");
        assert_eq!(report["comparisons"][0]["minSamples"], 2);
        value.currency = "USD<script>".into();
        assert!(value.validate().is_err());
    }
    #[test]
    fn save_is_atomic_idempotent_and_rejects_corrupt_store() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!(
            "jarvis-outcomes-{}-{}",
            std::process::id(),
            crate::util::now_ms()
        ));
        let v = serde_json::to_value(row()).unwrap();
        save(&dir, v.clone()).unwrap();
        save(&dir, v.clone()).unwrap();
        assert_eq!(load(&dir).unwrap().len(), 1);
        assert_eq!(
            fs::metadata(dir.join("analytics-outcomes.json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        fs::write(dir.join("analytics-outcomes.json"), b"broken").unwrap();
        assert!(save(&dir, v).is_err());
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn oversized_save_preserves_existing_readable_store() {
        let dir = std::env::temp_dir().join(format!(
            "jarvis-outcomes-size-{}-{}",
            std::process::id(),
            NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).unwrap();
        let mut sample = row();
        sample.reviewer_evidence = "x".repeat(4000);
        let count = (MAX_OUTCOME_BYTES - 2) / (serde_json::to_vec(&sample).unwrap().len() + 1) - 1;
        let items = vec![sample.clone(); count];
        let original = serde_json::to_vec(&items).unwrap();
        assert!(original.len() < MAX_OUTCOME_BYTES);
        // A compact valid store can exceed the same bound when saved prettily;
        // the limit must apply to actual serialized bytes, not just row count.
        assert!(serde_json::to_vec_pretty(&items).unwrap().len() > MAX_OUTCOME_BYTES);
        let path = dir.join("analytics-outcomes.json");
        fs::write(&path, &original).unwrap();
        assert_eq!(load(&dir).unwrap().len(), count);
        sample.session_id = "new-session".into();
        let error = save(&dir, serde_json::to_value(sample).unwrap()).unwrap_err();
        assert!(error.contains("8 МиБ"));
        assert_eq!(fs::read(&path).unwrap(), original);
        assert_eq!(load(&dir).unwrap().len(), count);
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn load_rejects_excessive_count_and_non_regular_store() {
        use std::os::unix::fs::symlink;
        let dir = std::env::temp_dir().join(format!(
            "jarvis-outcomes-count-{}-{}",
            std::process::id(),
            NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("analytics-outcomes.json");
        let bytes = serde_json::to_vec(&vec![row(); MAX_OUTCOMES + 1]).unwrap();
        assert!(bytes.len() < MAX_OUTCOME_BYTES);
        fs::write(&path, bytes).unwrap();
        assert!(load(&dir).unwrap_err().contains("10 000"));
        fs::remove_file(&path).unwrap();
        symlink(dir.join("missing"), &path).unwrap();
        assert!(load(&dir).is_err());
        fs::remove_dir_all(dir).unwrap();
    }
}

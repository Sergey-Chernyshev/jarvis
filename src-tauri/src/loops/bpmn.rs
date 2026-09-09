//! BPMN 2.0: тот же пайплайн, но файлом, который открывается в Camunda Modeler.
//!
//! Зачем вообще внешний редактор, когда конструктор уже есть. Затем, что
//! конструктор хорош на пяти узлах и беспомощен на двадцати пяти: списком
//! читается порядок, но не читается ФОРМА — где ветвление, куда возвращается
//! петля, что идёт параллельно. Нарисованный граф отвечает на это взглядом.
//! Своё полотно со стрелками писать незачем: bpmn-js уже написан, а Camunda
//! Modeler бесплатен и стоит у людей.
//!
//! Отсюда требование, которое и определило всё в этом файле: обмен ДВУСТОРОННИЙ
//! и без потерь. Не «красивая картинка на посмотреть», а рабочий файл — открыл,
//! перетащил, дорисовал ветку, сохранил, забрал обратно. Из этого следует:
//!
//! * координаты узлов живут в модели (`Step::x`/`y`): расстановка, сделанная
//!   руками, — работа человека, и автораскладка не имеет права её стирать;
//! * идентификатор Jarvis едет в файл отдельным свойством (`jarvis:id`), а не
//!   выводится из имени: переименование узла в модельере не должно ломать
//!   `${узел.вывод}` в чужих промтах;
//! * непонятое условие возвращается в файл ТЕМ ЖЕ текстом (`Cond::Expr`):
//!   упростить чужую правку до ближайшей знакомой — значит молча её потерять;
//! * промт лежит в `bpmn:documentation`, а не в своём теге, потому что
//!   документация — единственное поле, которое модельер даёт править у любого
//!   элемента. Всё остальное (модель, попытки, вид узла) — в
//!   `camunda:properties`, которые он показывает таблицей.
//!
//! Обратное чтение принимает и то, чего мы никогда не писали: человек рисует
//! в модельере обычные задачи и шлюзы, не зная про наши свойства. Такой узел
//! опознаётся по тегу BPMN, а идентификатор ему даётся от имени — чтобы
//! `${тесты.вывод}` заработало ровно так, как человек и ожидал.

use super::pipeline::{Cond, Flow, Pipeline, Step, StepKind};
use quick_xml::events::Event;
use quick_xml::Reader;
use std::collections::{HashMap, HashSet};

/* ======================= раскладка ======================= */

/// Отступ первой колонки и шаг между колонками/строками. Числа подобраны так,
/// чтобы дефолтная раскладка выглядела как нарисованная руками, а не как
/// «узлы вывалились кучей».
const X0: i32 = 240;
const DX: i32 = 200;
const Y0: i32 = 120;
const DY: i32 = 140;

/// Размеры элементов — те же, что рисует bpmn-js. Свои означали бы, что
/// открытая в модельере диаграмма «дёргается» при первом же сохранении.
const TASK: (i32, i32) = (100, 80);
const GATE: (i32, i32) = (50, 50);
const EVENT: (i32, i32) = (36, 36);

#[derive(Debug, Clone, Copy, PartialEq)]
struct Rect {
    x: i32,
    y: i32,
    w: i32,
    h: i32,
}

impl Rect {
    fn cx(&self) -> i32 {
        self.x + self.w / 2
    }
    fn cy(&self) -> i32 {
        self.y + self.h / 2
    }
    fn right(&self) -> i32 {
        self.x + self.w
    }
    fn bottom(&self) -> i32 {
        self.y + self.h
    }
}

fn size_of(kind: &StepKind) -> (i32, i32) {
    match kind {
        k if k.is_gateway() => GATE,
        StepKind::Wait { .. } => EVENT,
        _ => TASK,
    }
}

/// Разложить узлы по слоям обхода в ширину от старта.
///
/// Своя раскладка нужна ровно один раз — при первой выгрузке. Дальше координаты
/// приезжают из файла, и трогать их нельзя.
fn autolayout(p: &Pipeline) -> HashMap<String, (i32, i32)> {
    let mut level: HashMap<&str, usize> = HashMap::new();
    let mut order: Vec<&str> = Vec::new();
    if let Some(first) = p.first() {
        let mut queue = std::collections::VecDeque::from([first.id.as_str()]);
        level.insert(first.id.as_str(), 0);
        order.push(first.id.as_str());
        while let Some(id) = queue.pop_front() {
            let depth = level[id];
            let Some(s) = p.step(id) else { continue };
            for f in &s.next {
                if f.to.trim().is_empty() {
                    continue;
                }
                let to = f.to.as_str();
                if level.contains_key(to) {
                    continue; // уже размечен: обратная стрелка петли слой не двигает
                }
                level.insert(to, depth + 1);
                order.push(to);
                queue.push_back(to);
            }
        }
    }
    // Недостижимые узлы — тоже узлы: их надо ПОКАЗАТЬ, а не спрятать. Ставим
    // отдельной колонкой в конце, там их и видно как оторванные.
    let far = level.values().copied().max().map(|m| m + 2).unwrap_or(0);
    for s in &p.steps {
        if !level.contains_key(s.id.as_str()) {
            level.insert(s.id.as_str(), far);
            order.push(s.id.as_str());
        }
    }
    let mut used: HashMap<usize, i32> = HashMap::new();
    let mut out = HashMap::new();
    for id in order {
        let depth = level[id];
        let row = used.entry(depth).or_insert(0);
        let (w, h) = p.step(id).map(|s| size_of(&s.kind)).unwrap_or(TASK);
        // Центры колонок совпадают независимо от размера элемента — иначе
        // ромб шлюза висел бы выше задач соседней колонки.
        let x = X0 + depth as i32 * DX + (TASK.0 - w) / 2;
        let y = Y0 + *row * DY + (TASK.1 - h) / 2;
        *row += 1;
        out.insert(id.to_string(), (x, y));
    }
    out
}

/// Где каждый элемент стоит: сохранённые координаты в приоритете.
fn places(p: &Pipeline) -> HashMap<String, Rect> {
    let auto = autolayout(p);
    let mut out = HashMap::new();
    for s in &p.steps {
        let (w, h) = size_of(&s.kind);
        let (x, y) = if s.x != 0 || s.y != 0 {
            (s.x, s.y)
        } else {
            auto.get(&s.id).copied().unwrap_or((X0, Y0))
        };
        out.insert(s.id.clone(), Rect { x, y, w, h });
    }
    out
}

/* ======================= выгрузка ======================= */

const START: &str = "StartEvent_1";
const END: &str = "EndEvent_1";

/// Одна стрелка в терминах BPMN.
struct Edge {
    id: String,
    from: String,
    to: String,
    label: String,
    expr: String,
}

/// Пайплайн → BPMN 2.0 с диаграммой.
pub fn to_xml(p: &Pipeline, process_name: &str) -> String {
    let ids = bpmn_ids(p);
    let boxes = places(p);
    let edges = edges_of(p, &ids);

    // Стартовое и конечное события координат в модели не имеют: они служебные,
    // и держать их отдельными полями значило бы возить в JSON то, что всегда
    // выводится из соседей.
    let first_box = p.first().and_then(|s| boxes.get(&s.id)).copied();
    let start_rect = Rect {
        x: first_box.map(|r| r.x - 140).unwrap_or(X0 - 140),
        y: first_box.map(|r| r.cy() - EVENT.1 / 2).unwrap_or(Y0),
        w: EVENT.0,
        h: EVENT.1,
    };
    let right = boxes.values().map(|r| r.right()).max().unwrap_or(X0);
    let top = boxes.values().map(|r| r.y).min().unwrap_or(Y0);
    let end_rect = Rect { x: right + 100, y: top, w: EVENT.0, h: EVENT.1 };
    // Ключ — идентификатор BPMN, а не Jarvis: по нему смотрят и фигуры, и
    // стрелки. Один раз перепутанный ключ здесь означал выгрузку без диаграммы.
    let mut rects: HashMap<String, Rect> =
        boxes.iter().map(|(k, v)| (ids[k].clone(), *v)).collect();
    rects.insert(START.to_string(), start_rect);
    rects.insert(END.to_string(), end_rect);
    // Нижняя кромка нужна маршруту петли: обратная стрелка идёт ПОД схемой,
    // иначе она перечёркивает половину диаграммы.
    let floor = rects.values().map(|r| r.bottom()).max().unwrap_or(Y0) + 60;

    let mut x = String::with_capacity(4096);
    x.push_str(r#"<?xml version="1.0" encoding="UTF-8"?>"#);
    x.push('\n');
    x.push_str(
        r#"<bpmn:definitions xmlns:bpmn="http://www.omg.org/spec/BPMN/20100524/MODEL" xmlns:bpmndi="http://www.omg.org/spec/BPMN/20100524/DI" xmlns:dc="http://www.omg.org/spec/DD/20100524/DC" xmlns:di="http://www.omg.org/spec/DD/20100524/DI" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance" xmlns:camunda="http://camunda.org/schema/1.0/bpmn" id="Definitions_jarvis" targetNamespace="http://jarvis.local/loops" exporter="Jarvis" exporterVersion=""#,
    );
    x.push_str(env!("CARGO_PKG_VERSION"));
    x.push_str("\">\n");
    x.push_str(&format!(
        "  <bpmn:process id=\"Process_jarvis\" name=\"{}\" isExecutable=\"true\">\n",
        esc(process_name)
    ));

    // Старт.
    x.push_str(&format!("    <bpmn:startEvent id=\"{START}\" name=\"старт\">\n"));
    for e in edges.iter().filter(|e| e.from == START) {
        x.push_str(&format!("      <bpmn:outgoing>{}</bpmn:outgoing>\n", esc(&e.id)));
    }
    x.push_str("    </bpmn:startEvent>\n");

    for s in &p.steps {
        let id = &ids[&s.id];
        x.push_str(&element(s, id, &edges));
    }

    // Конец рисуем всегда: пайплайн без выхода — это ошибка, и её надо ВИДЕТЬ
    // на схеме, а не искать в списке претензий.
    x.push_str(&format!("    <bpmn:endEvent id=\"{END}\" name=\"конец\">\n"));
    for e in edges.iter().filter(|e| e.to == END) {
        x.push_str(&format!("      <bpmn:incoming>{}</bpmn:incoming>\n", esc(&e.id)));
    }
    x.push_str("    </bpmn:endEvent>\n");

    for e in &edges {
        let label = if e.label.is_empty() {
            String::new()
        } else {
            format!(" name=\"{}\"", esc(&e.label))
        };
        if e.expr.is_empty() {
            x.push_str(&format!(
                "    <bpmn:sequenceFlow id=\"{}\" sourceRef=\"{}\" targetRef=\"{}\"{label} />\n",
                esc(&e.id),
                esc(&e.from),
                esc(&e.to)
            ));
        } else {
            x.push_str(&format!(
                "    <bpmn:sequenceFlow id=\"{}\" sourceRef=\"{}\" targetRef=\"{}\"{label}>\n      \
                 <bpmn:conditionExpression xsi:type=\"bpmn:tFormalExpression\">{}</bpmn:conditionExpression>\n    \
                 </bpmn:sequenceFlow>\n",
                esc(&e.id),
                esc(&e.from),
                esc(&e.to),
                esc(&e.expr)
            ));
        }
    }
    x.push_str("  </bpmn:process>\n");

    /* ---- диаграмма ---- */
    x.push_str("  <bpmndi:BPMNDiagram id=\"Diagram_1\">\n    <bpmndi:BPMNPlane id=\"Plane_1\" bpmnElement=\"Process_jarvis\">\n");
    let shape = |id: &str, r: Rect, out: &mut String| {
        out.push_str(&format!(
            "      <bpmndi:BPMNShape id=\"{0}_di\" bpmnElement=\"{0}\">\n        \
             <dc:Bounds x=\"{1}\" y=\"{2}\" width=\"{3}\" height=\"{4}\" />\n      \
             </bpmndi:BPMNShape>\n",
            esc(id),
            r.x,
            r.y,
            r.w,
            r.h
        ));
    };
    shape(START, start_rect, &mut x);
    for s in &p.steps {
        let id = &ids[&s.id];
        shape(id, rects[id], &mut x);
    }
    shape(END, end_rect, &mut x);
    for e in &edges {
        let (Some(a), Some(b)) = (rects.get(e.from.as_str()), rects.get(e.to.as_str())) else {
            continue;
        };
        x.push_str(&format!(
            "      <bpmndi:BPMNEdge id=\"{0}_di\" bpmnElement=\"{0}\">\n",
            esc(&e.id)
        ));
        for (px, py) in waypoints(*a, *b, floor) {
            x.push_str(&format!("        <di:waypoint x=\"{px}\" y=\"{py}\" />\n"));
        }
        x.push_str("      </bpmndi:BPMNEdge>\n");
    }
    x.push_str("    </bpmndi:BPMNPlane>\n  </bpmndi:BPMNDiagram>\n</bpmn:definitions>\n");
    x
}

/// Один узел в терминах BPMN.
///
/// Тег выбирается по смыслу узла, а не «всё задачами»: человек, открывший файл,
/// должен видеть ромб там, где ветвление, и часы там, где пауза. Именно за это
/// сюда и ходят.
fn element(s: &Step, id: &str, edges: &[Edge]) -> String {
    let refs = |out: &mut String| {
        for e in edges.iter().filter(|e| e.to == id) {
            out.push_str(&format!("      <bpmn:incoming>{}</bpmn:incoming>\n", esc(&e.id)));
        }
        for e in edges.iter().filter(|e| e.from == id) {
            out.push_str(&format!("      <bpmn:outgoing>{}</bpmn:outgoing>\n", esc(&e.id)));
        }
    };
    let (tag, extra) = match &s.kind {
        StepKind::Agent { .. } => ("bpmn:serviceTask", " camunda:type=\"external\" camunda:topic=\"jarvis.agent\""),
        StepKind::Review { .. } => ("bpmn:serviceTask", " camunda:type=\"external\" camunda:topic=\"jarvis.review\""),
        StepKind::Shell { .. } => ("bpmn:scriptTask", " scriptFormat=\"shell\""),
        StepKind::Human { .. } => ("bpmn:userTask", ""),
        StepKind::Wait { .. } => ("bpmn:intermediateCatchEvent", ""),
        StepKind::Choice => ("bpmn:exclusiveGateway", ""),
        StepKind::Fork | StepKind::Join => ("bpmn:parallelGateway", ""),
    };
    // Переход «иначе» Camunda помечает атрибутом `default`, а не отсутствием
    // условия: так модельер рисует его косой чертой и не ругается на «два
    // безусловных выхода». Помечаем, только когда есть что противопоставить.
    //
    // И только там, где «иначе» вообще бывает: у задач и у развилки. У
    // ПАРАЛЛЕЛЬНОГО шлюза выбора нет по определению — уходят все ветки, — и
    // атрибут `default` на нём схему нарушает.
    let default = match &s.kind {
        StepKind::Fork | StepKind::Join => String::new(),
        _ => {
            let outs: Vec<&Edge> = edges.iter().filter(|e| e.from == id).collect();
            (outs.len() > 1)
                .then(|| outs.iter().rev().find(|e| e.expr.is_empty()))
                .flatten()
                .map(|e| format!(" default=\"{}\"", esc(&e.id)))
                .unwrap_or_default()
        }
    };
    let mut out = format!(
        "    <{tag} id=\"{}\" name=\"{}\"{extra}{default}>\n",
        esc(id),
        esc(&s.title())
    );
    // Документация — единственное поле, которое модельер даёт править у любого
    // элемента. Поэтому промт живёт именно здесь, а не в своём теге.
    let doc = match &s.kind {
        StepKind::Agent { prompt, .. } | StepKind::Review { prompt, .. } => prompt.clone(),
        StepKind::Shell { command } => command.clone(),
        StepKind::Human { question } => question.clone(),
        _ => String::new(),
    };
    if !doc.trim().is_empty() {
        out.push_str(&format!("      <bpmn:documentation>{}</bpmn:documentation>\n", esc(&doc)));
    }
    out.push_str("      <bpmn:extensionElements>\n        <camunda:properties>\n");
    let mut prop = |k: &str, v: &str| {
        out.push_str(&format!(
            "          <camunda:property name=\"{}\" value=\"{}\" />\n",
            esc(k),
            esc(v)
        ));
    };
    prop("jarvis:id", &s.id);
    prop("jarvis:kind", kind_word(&s.kind));
    match &s.kind {
        StepKind::Agent { model, .. } | StepKind::Review { model, .. } if !model.trim().is_empty() => {
            prop("jarvis:model", model)
        }
        _ => {}
    }
    if !s.on_conflict.trim().is_empty() {
        prop("jarvis:onConflict", &s.on_conflict);
    }
    if s.retries > 0 {
        prop("jarvis:retries", &s.retries.to_string());
    }
    out.push_str("        </camunda:properties>\n      </bpmn:extensionElements>\n");
    // Порядок детей задан схемой BPMN: documentation, extensionElements,
    // incoming, outgoing — и лишь затем script, multi-instance и определение
    // таймера. bpmn-js простит и другой, а проверяльщик схемы нет; файл должен
    // быть валидным, а не «обычно открывается».
    refs(&mut out);
    // «Для каждого» — это multi-instance, и рисуется он в модельере тремя
    // полосками под задачей. Параллельный (`isSequential="false"`), потому что
    // у нас каждый экземпляр идёт в своём рабочем дереве.
    if s.is_each() {
        out.push_str(&format!(
            "      <bpmn:multiInstanceLoopCharacteristics isSequential=\"false\" \
             camunda:collection=\"{}\" camunda:elementVariable=\"элемент\" />\n",
            esc(s.over.trim())
        ));
    }
    if let StepKind::Wait { minutes } = &s.kind {
        out.push_str(&format!(
            "      <bpmn:timerEventDefinition id=\"Timer_{}\">\n        \
             <bpmn:timeDuration xsi:type=\"bpmn:tFormalExpression\">PT{minutes}M</bpmn:timeDuration>\n      \
             </bpmn:timerEventDefinition>\n",
            esc(id)
        ));
    }
    if let StepKind::Shell { command } = &s.kind {
        out.push_str(&format!("      <bpmn:script>{}</bpmn:script>\n", esc(command)));
    }
    out.push_str(&format!("    </{tag}>\n"));
    out
}

fn kind_word(k: &StepKind) -> &'static str {
    match k {
        StepKind::Agent { .. } => "agent",
        StepKind::Shell { .. } => "shell",
        StepKind::Review { .. } => "review",
        StepKind::Human { .. } => "human",
        StepKind::Wait { .. } => "wait",
        StepKind::Choice => "choice",
        StepKind::Fork => "fork",
        StepKind::Join => "join",
    }
}

/// Идентификаторы элементов: свой у каждого узла, устойчивый между выгрузками.
fn bpmn_ids(p: &Pipeline) -> HashMap<String, String> {
    let mut taken: HashSet<String> = [START.to_string(), END.to_string()].into();
    let mut out = HashMap::new();
    for s in &p.steps {
        let mut id = ncname(&s.id);
        if taken.contains(&id) {
            for n in 2..999 {
                let candidate = format!("{id}_{n}");
                if !taken.contains(&candidate) {
                    id = candidate;
                    break;
                }
            }
        }
        taken.insert(id.clone());
        out.insert(s.id.clone(), id);
    }
    out
}

/// Идентификатор элемента BPMN: ТОЛЬКО ASCII.
///
/// Схема XML разрешает в идентификаторе любые буквы, и кириллица там законна.
/// На практике — нет: bpmn-js и Camunda Modeler стоят на `moddle`, а он
/// проверяет идентификаторы своим ASCII-шаблоном и отвечает «illegal ID».
/// Узел с русским идентификатором не просто теряет имя — он не разбирается
/// ЦЕЛИКОМ, и файл открывается пустым полотном. Проверено самим bpmn-moddle:
/// на графе из восьми узлов — сорок пять «unparsable content».
///
/// Поэтому в файл едет транслитерация, а настоящий идентификатор лежит рядом в
/// `jarvis:id` — по нему обратное чтение и возвращает граф один в один, так что
/// `${тесты.вывод}` в чужих промтах продолжает работать.
fn ncname(s: &str) -> String {
    let mut out = String::new();
    for ch in s.trim().chars() {
        match translit(ch) {
            Some(lat) => out.push_str(lat),
            None if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' || ch == '.' => out.push(ch),
            // Всё, чего мы не знаем, — подчёркивание. Молча выбросить символ
            // значило бы склеить два разных идентификатора в один.
            None => out.push('_'),
        }
    }
    if out.is_empty() {
        return "Node_1".into();
    }
    if !out.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_') {
        out.insert(0, '_');
    }
    out
}

/// Кириллица в латиницу. Таблица, а не крейт: тридцать три буквы не стоят
/// зависимости, а чужая таблица однажды поменяет правило и переименует всё.
fn translit(ch: char) -> Option<&'static str> {
    let lower = ch.to_lowercase().next().unwrap_or(ch);
    let lat = match lower {
        'а' => "a", 'б' => "b", 'в' => "v", 'г' => "g", 'д' => "d",
        'е' => "e", 'ё' => "e", 'ж' => "zh", 'з' => "z", 'и' => "i",
        'й' => "y", 'к' => "k", 'л' => "l", 'м' => "m", 'н' => "n",
        'о' => "o", 'п' => "p", 'р' => "r", 'с' => "s", 'т' => "t",
        'у' => "u", 'ф' => "f", 'х' => "h", 'ц' => "c", 'ч' => "ch",
        'ш' => "sh", 'щ' => "sch", 'ъ' => "", 'ы' => "y", 'ь' => "",
        'э' => "e", 'ю' => "yu", 'я' => "ya",
        _ => return None,
    };
    Some(lat)
}

fn edges_of(p: &Pipeline, ids: &HashMap<String, String>) -> Vec<Edge> {
    let mut out = Vec::new();
    let mut n = 0;
    let mut next_id = |flow: &Flow| -> String {
        n += 1;
        if flow.bpmn_id.trim().is_empty() {
            format!("Flow_{n}")
        } else {
            ncname(&flow.bpmn_id)
        }
    };
    if let Some(first) = p.first() {
        out.push(Edge {
            id: "Flow_start".into(),
            from: START.into(),
            to: ids[&first.id].clone(),
            label: String::new(),
            expr: String::new(),
        });
    }
    for s in &p.steps {
        for f in &s.next {
            let to = if f.to.trim().is_empty() {
                END.to_string()
            } else {
                match ids.get(&f.to) {
                    Some(x) => x.clone(),
                    None => continue, // ссылка в никуда: её ловит problems()
                }
            };
            out.push(Edge {
                id: next_id(f),
                from: ids[&s.id].clone(),
                to,
                label: flow_label(f),
                expr: super::pipeline::expr_of(&f.when),
            });
        }
    }
    out
}

/// Подпись стрелки. Своя у человека важнее нашей, но безымянное условие всё
/// равно надо подписать — иначе на схеме два одинаковых выхода из ромба.
fn flow_label(f: &Flow) -> String {
    if !f.label.trim().is_empty() {
        return f.label.clone();
    }
    match &f.when {
        Cond::Always => String::new(),
        Cond::Ok => "получилось".into(),
        Cond::Fail => "не вышло".into(),
        Cond::Verdict { verdict } => format!("вердикт {verdict}"),
        Cond::Contains { text } => format!("есть «{}»", crate::util::ellipsize(text.trim(), 24)),
        Cond::Expr { .. } => "по условию".into(),
    }
}

/// Маршрут стрелки. Три случая, и третий — тот, ради которого это вообще
/// пишется: обратная стрелка петли обязана идти ПОД схемой, иначе она
/// перечёркивает диаграмму по диагонали.
fn waypoints(a: Rect, b: Rect, floor: i32) -> Vec<(i32, i32)> {
    if b.x >= a.right() {
        if a.cy() == b.cy() {
            return vec![(a.right(), a.cy()), (b.x, b.cy())];
        }
        let mid = (a.right() + b.x) / 2;
        return vec![(a.right(), a.cy()), (mid, a.cy()), (mid, b.cy()), (b.x, b.cy())];
    }
    if a.x >= b.right() {
        // назад: вниз, под схемой, и вверх в цель
        return vec![
            (a.cx(), a.bottom()),
            (a.cx(), floor),
            (b.cx(), floor),
            (b.cx(), b.bottom()),
        ];
    }
    // колонки перекрываются — соединяем по вертикали
    if b.cy() >= a.cy() {
        vec![(a.cx(), a.bottom()), (b.cx(), b.y)]
    } else {
        vec![(a.cx(), a.y), (b.cx(), b.bottom())]
    }
}

fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(ch),
        }
    }
    out
}

/* ======================= разбор ======================= */

/// Узел, каким он лежит в файле, — до превращения в шаг пайплайна.
#[derive(Default, Debug)]
struct RawNode {
    tag: String,
    id: String,
    name: String,
    doc: String,
    script: String,
    timer: String,
    props: HashMap<String, String>,
    /// Переход по умолчанию (атрибут `default` у шлюза/задачи).
    default_flow: String,
    /// `camunda:collection` из multi-instance: список, по которому задача
    /// размножается. Пусто — обычная задача, один экземпляр.
    collection: String,
}

#[derive(Default, Debug)]
struct RawFlow {
    id: String,
    from: String,
    to: String,
    name: String,
    expr: String,
}

/// Куда складывать текст, который сейчас читается.
#[derive(PartialEq)]
enum Sink {
    None,
    Doc,
    Script,
    Timer,
    Cond,
}

/// BPMN 2.0 → пайплайн.
///
/// Принимаем не только свои файлы: человек рисует в модельере обычные задачи и
/// шлюзы, ничего не зная про наши свойства. Такой узел опознаётся по тегу, а
/// идентификатор ему даётся от ИМЕНИ — чтобы `${тесты.вывод}` заработало ровно
/// так, как человек и ожидал, увидев на схеме задачу «тесты».
pub fn from_xml(xml: &str) -> Result<Pipeline, String> {
    let mut reader = Reader::from_str(xml);
    // trim_text НЕ включаем: текст приезжает кусками (каждая сущность вроде
    // `&lt;` рвёт его на отдельные события), и обрезка съедала бы пробелы на
    // стыках. Пустые куски между тегами нам и так не мешают — приёмник у них
    // закрыт.
    reader.config_mut().trim_text(false);

    let mut nodes: Vec<RawNode> = Vec::new();
    let mut flows: Vec<RawFlow> = Vec::new();
    let mut shapes: HashMap<String, (i32, i32)> = HashMap::new();
    let mut sink = Sink::None;
    let mut shape_of: Option<String> = None;
    let mut in_process = false;

    loop {
        let ev = reader.read_event().map_err(|e| format!("xml: {e}"))?;
        match ev {
            Event::Eof => break,
            Event::Start(ref e) | Event::Empty(ref e) => {
                let empty = matches!(ev, Event::Empty(_));
                let tag = local(e.name().as_ref());
                let at = |name: &str| attr(e, name);
                match tag.as_str() {
                    "process" => in_process = true,
                    "sequenceFlow" if in_process => {
                        flows.push(RawFlow {
                            id: at("id"),
                            from: at("sourceRef"),
                            to: at("targetRef"),
                            name: at("name"),
                            expr: String::new(),
                        });
                    }
                    "documentation" if !empty => sink = Sink::Doc,
                    "script" if !empty => sink = Sink::Script,
                    "timeDuration" | "timeCycle" if !empty => sink = Sink::Timer,
                    "conditionExpression" if !empty => sink = Sink::Cond,
                    "property" => {
                        if let Some(n) = nodes.last_mut() {
                            let key = at("name");
                            if !key.is_empty() {
                                n.props.insert(key, at("value"));
                            }
                        }
                    }
                    // Multi-instance: у Camunda список лежит атрибутом
                    // `camunda:collection`. Это ровно наше «для каждого», и
                    // читать его надо, даже если файл рисовали не мы.
                    "multiInstanceLoopCharacteristics" => {
                        if let Some(n) = nodes.last_mut() {
                            let c = at("collection");
                            n.collection = if c.is_empty() { at("loopCardinality") } else { c };
                        }
                    }
                    "BPMNShape" => shape_of = Some(at("bpmnElement")),
                    "Bounds" => {
                        if let Some(id) = shape_of.take() {
                            let n = |v: String| v.split('.').next().unwrap_or("0").parse().unwrap_or(0);
                            shapes.insert(id, (n(at("x")), n(at("y"))));
                        }
                    }
                    _ if in_process && is_node_tag(&tag) => {
                        nodes.push(RawNode {
                            tag,
                            id: at("id"),
                            name: at("name"),
                            default_flow: at("default"),
                            ..Default::default()
                        });
                    }
                    _ => {}
                }
            }
            // Сущность (`&lt;`, `&#10;`) приезжает ОТДЕЛЬНЫМ событием, а не куском
            // текста. Не обработать её — значит тихо выбросить из промта каждый
            // символ, который пришлось экранировать: угловые скобки, амперсанды,
            // кавычки. Именно так «почини <div> & "кавычки"» превращалось в
            // «почини div кавычки».
            Event::GeneralRef(r) => {
                if sink == Sink::None {
                    continue;
                }
                let name = r.decode().map_err(|e| format!("xml: {e}"))?;
                let text = entity(&name);
                push_text(&mut nodes, &mut flows, &sink, &text);
            }
            Event::Text(t) => {
                if sink == Sink::None {
                    continue;
                }
                let text = t.xml_content().map_err(|e| format!("xml: {e}"))?.into_owned();
                push_text(&mut nodes, &mut flows, &sink, &text);
            }
            Event::End(ref e) => {
                let tag = local(e.name().as_ref());
                if tag == "process" {
                    in_process = false;
                }
                if matches!(tag.as_str(), "documentation" | "script" | "timeDuration" | "timeCycle" | "conditionExpression")
                {
                    sink = Sink::None;
                }
            }
            _ => {}
        }
    }

    if nodes.is_empty() && flows.is_empty() {
        return Err("в файле нет процесса BPMN — это точно .bpmn?".into());
    }
    assemble(nodes, flows, shapes)
}

/// Дописать кусок текста туда, куда он сейчас читается.
///
/// Именно ДОПИСАТЬ: текст приезжает частями — каждая сущность рвёт его на
/// отдельные события, — и присваивание оставило бы от промта последний кусок.
fn push_text(nodes: &mut [RawNode], flows: &mut [RawFlow], sink: &Sink, text: &str) {
    match sink {
        Sink::Doc => set(nodes.last_mut(), |n| n.doc.push_str(text)),
        Sink::Script => set(nodes.last_mut(), |n| n.script.push_str(text)),
        Sink::Timer => set(nodes.last_mut(), |n| n.timer.push_str(text)),
        Sink::Cond => set(flows.last_mut(), |f| f.expr.push_str(text)),
        Sink::None => {}
    }
}

/// Сущность XML в символ. Пять предопределённых плюс числовые — больше в BPMN
/// и не встречается. Непонятую возвращаем КАК ЕСТЬ: выбросить символ молча
/// хуже, чем оставить `&что-то;` на виду.
fn entity(name: &str) -> String {
    match name {
        "lt" => "<".into(),
        "gt" => ">".into(),
        "amp" => "&".into(),
        "quot" => "\"".into(),
        "apos" => "'".into(),
        _ => {
            let code = match name.strip_prefix('#') {
                Some(hex) if hex.starts_with('x') || hex.starts_with('X') => {
                    u32::from_str_radix(&hex[1..], 16).ok()
                }
                Some(dec) => dec.parse::<u32>().ok(),
                None => None,
            };
            match code.and_then(char::from_u32) {
                Some(ch) => ch.to_string(),
                None => format!("&{name};"),
            }
        }
    }
}

fn set<T>(slot: Option<&mut T>, f: impl FnOnce(&mut T)) {
    if let Some(x) = slot {
        f(x);
    }
}

/// Теги, которые мы считаем узлами процесса.
fn is_node_tag(tag: &str) -> bool {
    matches!(
        tag,
        "startEvent"
            | "endEvent"
            | "task"
            | "serviceTask"
            | "scriptTask"
            | "userTask"
            | "manualTask"
            | "sendTask"
            | "receiveTask"
            | "businessRuleTask"
            | "callActivity"
            | "subProcess"
            | "intermediateCatchEvent"
            | "intermediateThrowEvent"
            | "exclusiveGateway"
            | "parallelGateway"
            | "inclusiveGateway"
            | "eventBasedGateway"
            | "complexGateway"
    )
}

fn assemble(
    nodes: Vec<RawNode>,
    flows: Vec<RawFlow>,
    shapes: HashMap<String, (i32, i32)>,
) -> Result<Pipeline, String> {
    let starts: HashSet<&str> = nodes
        .iter()
        .filter(|n| n.tag == "startEvent")
        .map(|n| n.id.as_str())
        .collect();
    let ends: HashSet<&str> = nodes
        .iter()
        .filter(|n| n.tag == "endEvent")
        .map(|n| n.id.as_str())
        .collect();

    // Степени нужны, чтобы отличить ветвление от слияния: у обоих в BPMN один
    // и тот же ромб с плюсом, и различает их только форма графа.
    let mut outs: HashMap<&str, usize> = HashMap::new();
    let mut ins: HashMap<&str, usize> = HashMap::new();
    for f in &flows {
        *outs.entry(f.from.as_str()).or_default() += 1;
        *ins.entry(f.to.as_str()).or_default() += 1;
    }

    // bpmn-id → jarvis-id. Своё свойство в приоритете: переименование узла в
    // модельере не должно ломать `${узел.вывод}` в чужих промтах.
    let mut jid: HashMap<String, String> = HashMap::new();
    let mut taken: HashSet<String> = HashSet::new();
    for n in &nodes {
        if starts.contains(n.id.as_str()) || ends.contains(n.id.as_str()) {
            continue;
        }
        let want = n
            .props
            .get("jarvis:id")
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .or_else(|| Some(id_from_name(&n.name)).filter(|s| !s.is_empty()))
            .unwrap_or_else(|| n.id.clone());
        let mut id = want.clone();
        for k in 2..999 {
            if taken.insert(id.clone()) {
                break;
            }
            id = format!("{want}-{k}");
        }
        jid.insert(n.id.clone(), id);
    }

    let mut steps: Vec<Step> = Vec::new();
    let mut start = String::new();
    for n in &nodes {
        if ends.contains(n.id.as_str()) {
            continue;
        }
        if starts.contains(n.id.as_str()) {
            // Куда ведёт старт — тот узел и первый. Несколько стартовых событий
            // BPMN разрешает; берём первый, остальные всплывут недостижимыми.
            if start.is_empty() {
                if let Some(f) = flows.iter().find(|f| f.from == n.id) {
                    start = jid.get(&f.to).cloned().unwrap_or_default();
                }
            }
            continue;
        }
        let id = jid[&n.id].clone();
        let kind = kind_of(n, outs.get(n.id.as_str()).copied().unwrap_or(0), ins.get(n.id.as_str()).copied().unwrap_or(0));
        let mut next: Vec<Flow> = flows
            .iter()
            .filter(|f| f.from == n.id)
            .map(|f| {
                let to = if ends.contains(f.to.as_str()) {
                    String::new()
                } else {
                    jid.get(&f.to).cloned().unwrap_or_default()
                };
                // Переход по умолчанию у Camunda — стрелка без условия, помеченная
                // атрибутом `default`. У нас это «всегда», и он обязан быть
                // последним, иначе съел бы все остальные ветки.
                let when = if !n.default_flow.is_empty() && n.default_flow == f.id {
                    Cond::Always
                } else {
                    parse_cond(&f.expr)
                };
                Flow { to, label: f.name.clone(), bpmn_id: f.id.clone(), when }
            })
            .collect();
        // Безусловные переходы — вниз списка: «первый подходящий выигрывает», и
        // «всегда» посреди списка означал бы, что условные ниже не сработают.
        next.sort_by_key(|f| matches!(f.when, Cond::Always));
        let (x, y) = shapes.get(&n.id).copied().unwrap_or((0, 0));
        let mut step = Step::node(&id, kind, next);
        step.name = n.name.clone();
        step.retries = n.props.get("jarvis:retries").and_then(|v| v.trim().parse().ok()).unwrap_or(0);
        step.on_conflict = n.props.get("jarvis:onConflict").cloned().unwrap_or_default();
        step.over = n.collection.trim().to_string();
        (step.x, step.y) = shapes.get(&n.id).copied().unwrap_or((x, y));
        steps.push(step);
    }

    if steps.is_empty() {
        return Err("в процессе нет ни одной задачи".into());
    }
    if start.is_empty() {
        start = steps[0].id.clone();
    }
    Ok(Pipeline { start, steps, bpmn_file: String::new(), bpmn_mtime: 0 })
}

/// Вид узла: своё свойство в приоритете, иначе — по тегу BPMN.
fn kind_of(n: &RawNode, outs: usize, ins: usize) -> StepKind {
    let doc = if n.doc.trim().is_empty() { n.script.trim() } else { n.doc.trim() }.to_string();
    let model = n.props.get("jarvis:model").cloned().unwrap_or_default();
    match n.props.get("jarvis:kind").map(|s| s.trim()) {
        Some("agent") => return StepKind::Agent { prompt: doc, model },
        Some("shell") => return StepKind::Shell { command: doc },
        Some("review") => return StepKind::Review { prompt: doc, model },
        Some("human") => return StepKind::Human { question: doc },
        Some("wait") => return StepKind::Wait { minutes: minutes_of(&n.timer) },
        Some("choice") => return StepKind::Choice,
        Some("fork") => return StepKind::Fork,
        Some("join") => {
            return StepKind::Join
        }
        _ => {}
    }
    match n.tag.as_str() {
        "scriptTask" => StepKind::Shell { command: doc },
        "userTask" | "manualTask" | "receiveTask" => StepKind::Human { question: doc },
        "intermediateCatchEvent" | "intermediateThrowEvent" => StepKind::Wait { minutes: minutes_of(&n.timer) },
        "exclusiveGateway" | "eventBasedGateway" | "complexGateway" => StepKind::Choice,
        // Ромб с плюсом — и ветвление, и слияние. Отличает их только форма
        // графа: расходится или сходится.
        "parallelGateway" | "inclusiveGateway" => {
            if ins > 1 && outs <= 1 {
                StepKind::Join
            } else if outs > 1 {
                StepKind::Fork
            } else {
                StepKind::Choice
            }
        }
        _ => StepKind::Agent { prompt: doc, model },
    }
}

/// `PT30M`, `PT2H`, `PT1H30M` → минуты. Не ISO-разбор целиком: суток и месяцев
/// у паузы шага не бывает, а притворяться, что мы их понимаем, — врать.
fn minutes_of(timer: &str) -> u32 {
    let t = timer.trim().to_uppercase();
    let Some(rest) = t.strip_prefix("PT") else {
        return 5;
    };
    let mut minutes = 0u32;
    let mut num = String::new();
    for ch in rest.chars() {
        if ch.is_ascii_digit() {
            num.push(ch);
            continue;
        }
        let n: u32 = num.parse().unwrap_or(0);
        num.clear();
        match ch {
            'H' => minutes += n * 60,
            'M' => minutes += n,
            'S' => minutes += n / 60,
            _ => {}
        }
    }
    if minutes == 0 {
        5
    } else {
        minutes
    }
}

/// Выражение перехода в условие. Непонятое остаётся ТЕКСТОМ: упростить чужую
/// правку до ближайшей знакомой — значит молча её потерять.
fn parse_cond(expr: &str) -> Cond {
    if expr.trim().is_empty() {
        return Cond::Always;
    }
    super::pipeline::parse_expr(expr).unwrap_or(Cond::Expr { text: expr.trim().to_string() })
}

/// Идентификатор от имени узла — для того, что человек нарисовал сам.
fn id_from_name(name: &str) -> String {
    let mut out = String::new();
    let mut dash = false;
    for ch in name.trim().chars() {
        if ch.is_alphanumeric() {
            out.push(ch.to_lowercase().next().unwrap_or(ch));
            dash = false;
        } else if !dash && !out.is_empty() {
            out.push('-');
            dash = true;
        }
    }
    out.trim_matches('-').chars().take(32).collect()
}

fn local(name: &[u8]) -> String {
    let s = String::from_utf8_lossy(name);
    s.rsplit(':').next().unwrap_or(&s).to_string()
}

fn attr(e: &quick_xml::events::BytesStart, want: &str) -> String {
    for a in e.attributes().flatten() {
        if local(a.key.as_ref()) == want {
            return a.unescape_value().map(|c| c.into_owned()).unwrap_or_default();
        }
    }
    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent(id: &str, prompt: &str, next: Vec<Flow>) -> Step {
        Step::node(id, StepKind::Agent { prompt: prompt.into(), model: String::new() }, next)
    }

    fn sample() -> Pipeline {
        Pipeline {
            start: "разведка".into(),
            steps: vec![
                agent("разведка", "разберись", vec![Flow::to("правка", Cond::Always)]),
                agent("правка", "сделай по плану: ${разведка.вывод}", vec![Flow::to("тесты", Cond::Always)]),
                Step::node("тесты", StepKind::Shell { command: "cargo test".into() }, vec![Flow::to("правка", Cond::Fail), Flow::to("", Cond::Always)]),
            ],
            ..Default::default()
        }
    }

    #[test]
    fn the_export_is_a_bpmn_file_a_modeler_can_open() {
        let xml = to_xml(&sample(), "ночной прогон");
        // Пространства имён и корень — без них модельер файл не откроет вовсе.
        assert!(xml.starts_with("<?xml"), "{}", &xml[..40]);
        assert!(xml.contains("bpmn:definitions"));
        assert!(xml.contains(r#"xmlns:bpmndi="http://www.omg.org/spec/BPMN/20100524/DI""#));
        // Диаграмма обязательна: без DI bpmn-js покажет пустое полотно.
        assert!(xml.contains("<bpmndi:BPMNDiagram"), "нет диаграммы");
        assert!(xml.contains("<dc:Bounds"), "нет координат");
        assert!(xml.contains("<di:waypoint"), "нет маршрутов стрелок");
        // Смысл узла виден по тегу — за этим в модельер и ходят.
        assert!(xml.contains("bpmn:serviceTask"), "агент — задача");
        assert!(xml.contains("bpmn:scriptTask"), "команда — скрипт-задача");
        assert!(xml.contains("<bpmn:startEvent") && xml.contains("<bpmn:endEvent"));
        // Промт правится в модельере как документация.
        assert!(xml.contains("<bpmn:documentation>разберись</bpmn:documentation>"));
        assert!(xml.contains(r#"name="jarvis:kind" value="agent""#));
    }

    /// Главное свойство обмена: выгрузили — забрали обратно — получили то же
    /// самое. Иначе первый же круг через модельер тихо съедает часть графа.
    #[test]
    fn a_round_trip_changes_nothing() {
        let before = sample();
        let xml = to_xml(&before, "прогон");
        let after = from_xml(&xml).expect("файл не разобрался");
        assert_eq!(after.start, before.start);
        assert_eq!(after.steps.len(), before.steps.len());
        for (a, b) in after.steps.iter().zip(before.steps.iter()) {
            assert_eq!(a.id, b.id, "идентификатор — ключ переменных, он обязан выжить");
            assert_eq!(a.kind, b.kind);
            assert_eq!(a.retries, b.retries);
            let tos: Vec<&str> = a.next.iter().map(|f| f.to.as_str()).collect();
            let was: Vec<&str> = b.next.iter().map(|f| f.to.as_str()).collect();
            assert_eq!(tos, was, "переходы {}", a.id);
            for (fa, fb) in a.next.iter().zip(b.next.iter()) {
                assert_eq!(fa.when, fb.when);
            }
        }
    }

    /// Параллель — то, ради чего это всё: ветвление и слияние обязаны стать
    /// ромбами с плюсом и вернуться собой.
    #[test]
    fn parallel_gateways_survive_the_round_trip() {
        let p = Pipeline {
            start: "разойтись".into(),
            steps: vec![
                Step::node("разойтись", StepKind::Fork, vec![Flow::to("фронт", Cond::Always), Flow::to("бэк", Cond::Always)]),
                agent("фронт", "правь фронт", vec![Flow::to("свести", Cond::Always)]),
                agent("бэк", "правь бэк", vec![Flow::to("свести", Cond::Always)]),
                Step::node("свести", StepKind::Join, vec![Flow::to("", Cond::Always)]),
            ],
            ..Default::default()
        };
        let xml = to_xml(&p, "параллель");
        assert_eq!(xml.matches("bpmn:parallelGateway").count(), 4, "два шлюза = четыре тега");
        let back = from_xml(&xml).unwrap();
        assert_eq!(back.step("разойтись").unwrap().kind, StepKind::Fork);
        assert_eq!(
            back.step("свести").unwrap().kind,
            StepKind::Join,
            "решение конфликта — свойство узла, терять его нельзя"
        );
        assert!(back.problems().is_empty(), "{:?}", back.problems());
    }

    /// Расстановка, сделанная руками в модельере, — работа человека. Стереть её
    /// автораскладкой значит заставить его делать её каждый раз заново.
    #[test]
    fn hand_made_layout_is_kept() {
        let mut p = sample();
        p.steps[1].x = 777;
        p.steps[1].y = 333;
        let xml = to_xml(&p, "прогон");
        assert!(xml.contains(r#"x="777" y="333""#), "координаты не доехали в файл");
        let back = from_xml(&xml).unwrap();
        let s = back.step("правка").unwrap();
        assert_eq!((s.x, s.y), (777, 333), "координаты не вернулись из файла");
    }

    /// Файл, нарисованный человеком с нуля: ни одного нашего свойства. Узлы
    /// опознаются по тегам, а идентификаторы берутся от имён — чтобы
    /// `${тесты.вывод}` заработало так, как он и ожидал.
    #[test]
    fn a_diagram_drawn_by_hand_is_understood() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<bpmn:definitions xmlns:bpmn="http://www.omg.org/spec/BPMN/20100524/MODEL">
  <bpmn:process id="Process_1" isExecutable="true">
    <bpmn:startEvent id="StartEvent_1" />
    <bpmn:serviceTask id="Activity_0aa" name="правка">
      <bpmn:documentation>почини сборку</bpmn:documentation>
    </bpmn:serviceTask>
    <bpmn:scriptTask id="Activity_0bb" name="тесты">
      <bpmn:script>cargo test</bpmn:script>
    </bpmn:scriptTask>
    <bpmn:exclusiveGateway id="Gateway_0cc" name="зелено?" default="Flow_4" />
    <bpmn:endEvent id="Event_0dd" />
    <bpmn:sequenceFlow id="Flow_1" sourceRef="StartEvent_1" targetRef="Activity_0aa" />
    <bpmn:sequenceFlow id="Flow_2" sourceRef="Activity_0aa" targetRef="Activity_0bb" />
    <bpmn:sequenceFlow id="Flow_3" sourceRef="Activity_0bb" targetRef="Gateway_0cc" />
    <bpmn:sequenceFlow id="Flow_4" sourceRef="Gateway_0cc" targetRef="Event_0dd" />
    <bpmn:sequenceFlow id="Flow_5" sourceRef="Gateway_0cc" targetRef="Activity_0aa">
      <bpmn:conditionExpression xsi:type="bpmn:tFormalExpression">${fail}</bpmn:conditionExpression>
    </bpmn:sequenceFlow>
  </bpmn:process>
</bpmn:definitions>"#;
        let p = from_xml(xml).unwrap();
        assert_eq!(p.start, "правка", "старт ведёт в первую задачу");
        assert_eq!(p.steps.len(), 3, "стартовое и конечное события узлами не считаются");
        assert_eq!(
            p.step("тесты").unwrap().kind,
            StepKind::Shell { command: "cargo test".into() },
            "скрипт-задача — команда, текст из <script>"
        );
        assert_eq!(p.step("зелено").unwrap().kind, StepKind::Choice);
        // Порядок переходов решает: условный обязан стоять ВЫШЕ «всегда»,
        // иначе безусловный съест ветку возврата.
        let g = p.step("зелено").unwrap();
        assert_eq!(g.next[0].when, Cond::Fail);
        assert_eq!(g.next[0].to, "правка");
        assert_eq!(g.next[1].when, Cond::Always);
        assert_eq!(g.next[1].to, "", "переход в конечное событие — это конец");
    }

    /// Переименование узла в модельере не должно ломать `${узел.вывод}` в
    /// чужих промтах: идентификатор мы записали своим свойством, и оно главнее
    /// имени.
    #[test]
    fn renaming_in_the_modeler_does_not_break_variables() {
        let xml = to_xml(&sample(), "прогон").replace(r#"name="тесты""#, r#"name="прогон тестов""#);
        let back = from_xml(&xml).unwrap();
        assert!(back.step("тесты").is_some(), "идентификатор пережил переименование");
        assert_eq!(back.step("тесты").unwrap().name, "прогон тестов");
        assert!(back.steps.iter().any(|s| s.next.iter().any(|f| f.to == "тесты")));
    }

    /// «Иначе» помечается атрибутом `default` — так его рисует модельер, и так
    /// он перестаёт считать два выхода из ромба одинаково безусловными.
    #[test]
    fn the_else_branch_is_marked_as_the_default_flow() {
        let xml = to_xml(&sample(), "прогон");
        let default = xml
            .lines()
            .find(|l| l.contains("scriptTask") && l.contains("default="))
            .expect("переход «иначе» не помечен");
        let id = default.split("default=\"").nth(1).unwrap().split('"').next().unwrap();
        // Помеченная стрелка — именно та, у которой нет условия.
        let flow = xml.lines().find(|l| l.contains(&format!("sequenceFlow id=\"{id}\""))).unwrap();
        assert!(flow.ends_with("/>"), "у «иначе» не должно быть условия: {flow}");
        // У узла с единственным выходом помечать нечего.
        assert!(!xml.contains("id=\"разведка\" name=\"разведка\" camunda:type=\"external\" camunda:topic=\"jarvis.agent\" default="));
        // А у параллельного шлюза «иначе» не бывает вовсе: уходят все ветки.
        let par = Pipeline {
            start: "разойтись".into(),
            steps: vec![
                Step::node("разойтись", StepKind::Fork, vec![Flow::to("a", Cond::Always), Flow::to("b", Cond::Always)]),
                agent("a", "x", vec![Flow::to("", Cond::Always)]),
                agent("b", "y", vec![Flow::to("", Cond::Always)]),
            ],
            ..Default::default()
        };
        let px = to_xml(&par, "п");
        assert!(!px.contains("parallelGateway id=\"разойтись\" name=\"разойтись\" default="), "{px}");
        // И круг через файл этого не теряет.
        let back = from_xml(&xml).unwrap();
        let tests = back.step("тесты").unwrap();
        assert_eq!(tests.next.last().unwrap().when, Cond::Always);
        assert_eq!(tests.next.last().unwrap().to, "");
    }

    /// «Для каждого» — это multi-instance из BPMN, и в модельере оно рисуется
    /// тремя полосками под задачей. Значит и в файле должно быть им, а не
    /// нашим свойством сбоку.
    #[test]
    fn each_becomes_a_multi_instance_activity() {
        let mut s = agent("править", "почини ${элемент}", vec![Flow::to("", Cond::Always)]);
        s.over = "${тесты.вывод}".into();
        s.on_conflict = "agent".into();
        let p = Pipeline { start: "править".into(), steps: vec![s], ..Default::default() };
        let xml = to_xml(&p, "прогон");
        assert!(xml.contains("<bpmn:multiInstanceLoopCharacteristics"), "{xml}");
        assert!(xml.contains(r#"isSequential="false""#), "экземпляры идут параллельно");
        assert!(xml.contains(r#"camunda:collection="${тесты.вывод}""#), "список не доехал");
        assert!(xml.contains(r#"camunda:elementVariable="элемент""#));

        let back = from_xml(&xml).unwrap();
        let s = back.step("править").unwrap();
        assert_eq!(s.over, "${тесты.вывод}", "список не вернулся");
        assert_eq!(s.on_conflict, "agent", "правило конфликта не вернулось");
        assert!(s.is_each());
        // И второй круг ничего не двигает.
        assert_eq!(xml, to_xml(&back, "прогон"));
    }

    /// Файл, нарисованный человеком: он ставит multi-instance кнопкой в
    /// модельере и пишет коллекцию в свойствах. Ничего про нас не зная.
    #[test]
    fn a_hand_made_multi_instance_is_understood() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<bpmn:definitions xmlns:bpmn="http://www.omg.org/spec/BPMN/20100524/MODEL" xmlns:camunda="http://camunda.org/schema/1.0/bpmn">
  <bpmn:process id="Process_1">
    <bpmn:startEvent id="StartEvent_1" />
    <bpmn:serviceTask id="Activity_1" name="править">
      <bpmn:documentation>почини ${элемент}</bpmn:documentation>
      <bpmn:multiInstanceLoopCharacteristics isSequential="false" camunda:collection="${тесты.вывод}" camunda:elementVariable="элемент" />
    </bpmn:serviceTask>
    <bpmn:sequenceFlow id="F1" sourceRef="StartEvent_1" targetRef="Activity_1" />
  </bpmn:process>
</bpmn:definitions>"#;
        let p = from_xml(xml).unwrap();
        let s = p.step("править").unwrap();
        assert!(s.is_each(), "multi-instance не опознан");
        assert_eq!(s.over, "${тесты.вывод}");
    }

    /// Чужое выражение возвращается в файл ровно тем же текстом.
    #[test]
    fn an_alien_expression_survives_untouched() {
        let xml = to_xml(&sample(), "прогон")
            .replace("${fail}", "${myBean.decide(execution)}");
        let back = from_xml(&xml).unwrap();
        let f = &back.step("тесты").unwrap().next[0];
        assert_eq!(f.when, Cond::Expr { text: "${myBean.decide(execution)}".into() });
        let again = to_xml(&back, "прогон");
        assert!(again.contains("${myBean.decide(execution)}"), "выражение потеряно");
    }

    #[test]
    fn a_pause_is_a_timer_event() {
        let p = Pipeline {
            start: "ждём".into(),
            steps: vec![Step::node("ждём", StepKind::Wait { minutes: 90 }, vec![Flow::to("", Cond::Always)])],
            ..Default::default()
        };
        let xml = to_xml(&p, "прогон");
        assert!(xml.contains("timerEventDefinition") && xml.contains("PT90M"), "{xml}");
        assert_eq!(from_xml(&xml).unwrap().step("ждём").unwrap().kind, StepKind::Wait { minutes: 90 });
        // Часы с минутами человек напишет сам — понимаем и их.
        assert_eq!(minutes_of("PT1H30M"), 90);
        assert_eq!(minutes_of("PT2H"), 120);
        assert_eq!(minutes_of("мусор"), 5, "непонятое — не ноль: пауза в ноль минут не пауза");
    }

    /// Идентификаторы в файле обязаны быть ASCII — иначе moddle, на котором
    /// стоят bpmn-js и Camunda Modeler, отвечает «illegal ID» и не разбирает
    /// узел ЦЕЛИКОМ. Файл открывается пустым полотном, и понять почему —
    /// отдельный вечер.
    #[test]
    fn every_id_in_the_file_is_ascii() {
        let xml = to_xml(&sample(), "прогон");
        for line in xml.lines() {
            for at in line.match_indices("id=\"") {
                let id: String = line[at.0 + 4..].chars().take_while(|c| *c != '"').collect();
                assert!(
                    id.is_ascii(),
                    "идентификатор «{id}» не ASCII — Camunda Modeler не откроет файл"
                );
            }
        }
        // Настоящее имя при этом никуда не делось и возвращается обратно.
        assert!(xml.contains(r#"name="jarvis:id" value="тесты""#), "{xml}");
        assert!(from_xml(&xml).unwrap().step("тесты").is_some());
    }

    #[test]
    fn ids_with_spaces_become_valid_xml_names() {
        assert_eq!(ncname("прогон тестов"), "progon_testov");
        assert_eq!(ncname("2шаг"), "_2shag", "имя XML не может начинаться с цифры");
        assert_eq!(ncname("щи-ёж"), "schi-ezh");
        assert_eq!(ncname(""), "Node_1");
        // А в файле рядом лежит настоящий идентификатор — по нему и вернём.
        let p = Pipeline {
            start: "мой шаг".into(),
            steps: vec![agent("мой шаг", "делай", vec![Flow::to("", Cond::Always)])],
            ..Default::default()
        };
        let back = from_xml(&to_xml(&p, "x")).unwrap();
        assert_eq!(back.steps[0].id, "мой шаг");
    }

    /// Круг через файл ничего не должен ДВИГАТЬ: второй прогон обязан дать
    /// байт в байт тот же файл. Иначе каждое открытие в модельере показывало бы
    /// «изменения», которых человек не делал, — и разбирать, где его правка, а
    /// где наша, стало бы невозможно.
    #[test]
    fn exporting_what_we_imported_gives_the_very_same_file() {
        let node = Step::node;
        let p = Pipeline {
            start: "план".into(),
            steps: vec![
                node("план", StepKind::Agent { prompt: "разбей задачу".into(), model: "opus".into() }, vec![Flow::to("разойтись", Cond::Always)]),
                node("разойтись", StepKind::Fork, vec![Flow::to("фронт", Cond::Always), Flow::to("бэк", Cond::Always)]),
                node("фронт", StepKind::Agent { prompt: "правь фронт: ${план.вывод}".into(), model: String::new() }, vec![Flow::to("свести", Cond::Always)]),
                node("бэк", StepKind::Agent { prompt: "правь бэк".into(), model: String::new() }, vec![Flow::to("свести", Cond::Always)]),
                node("свести", StepKind::Join, vec![Flow::to("тесты", Cond::Ok), Flow::to("", Cond::Always)]),
                node("тесты", StepKind::Shell { command: "cargo test".into() }, vec![Flow::to("развилка", Cond::Always)]),
                node("развилка", StepKind::Choice, vec![Flow::to("починить", Cond::Fail), Flow::to("ждём", Cond::Always)]),
                node("починить", StepKind::Agent { prompt: "почини: ${тесты.вывод}".into(), model: String::new() }, vec![Flow::to("тесты", Cond::Always)]),
                node("ждём", StepKind::Wait { minutes: 30 }, vec![Flow::to("спросить", Cond::Always)]),
                node("спросить", StepKind::Human { question: "выкатываем?".into() }, vec![Flow::to("", Cond::Always)]),
            ],
            ..Default::default()
        };
        let once = to_xml(&p, "ночной прогон");
        let twice = to_xml(&from_xml(&once).unwrap(), "ночной прогон");
        assert_eq!(once, twice, "второй круг через файл изменил его");
        // И заодно: в файле есть всё, что рисует модельер, — по одному тегу на
        // каждый вид узла этого графа.
        for tag in [
            "bpmn:startEvent", "bpmn:endEvent", "bpmn:serviceTask", "bpmn:scriptTask",
            "bpmn:userTask", "bpmn:intermediateCatchEvent", "bpmn:exclusiveGateway",
            "bpmn:parallelGateway", "bpmn:sequenceFlow", "bpmn:conditionExpression",
        ] {
            assert!(once.contains(tag), "в файле нет {tag}");
        }
    }

    /// Файл, переписанный САМИМ модельером, обязан читаться нами в тот же
    /// пайплайн. Это и есть настоящий круг: выгрузили → открыли в Camunda →
    /// сохранили → забрали обратно. Наши собственные тесты этого не покажут:
    /// они проверяют, что мы читаем то, что сами и написали.
    ///
    /// Слепок снят `scripts/check-bpmn.mjs --rewrite` с заготовки
    /// «параллельная правка»; обновлять его руками не надо — перегенерируй.
    #[test]
    fn a_file_rewritten_by_the_modeler_reads_back_the_same() {
        let xml = include_str!("testdata/camunda-rewritten.bpmn");
        let p = from_xml(xml).expect("модельер переписал — а мы не прочли");
        assert!(p.problems().is_empty(), "{:?}", p.problems());
        // Идентификаторы вернулись настоящими, а не транслитерацией.
        for id in ["план", "разойтись", "первая", "вторая", "свести", "слилось", "тесты"] {
            assert!(p.step(id).is_some(), "потерян шаг «{id}»: {:?}",
                    p.steps.iter().map(|s| s.id.clone()).collect::<Vec<_>>());
        }
        assert_eq!(p.start, "план");
        assert_eq!(p.step("разойтись").unwrap().kind, StepKind::Fork);
        assert_eq!(p.step("свести").unwrap().kind, StepKind::Join);
        assert_eq!(p.step("свести").unwrap().on_conflict, "agent");
        assert_eq!(p.step("слилось").unwrap().kind, StepKind::Choice);
        // И переменные в промтах уцелели — ради них идентификатор и возится
        // отдельным свойством.
        let fix = p.step("первая").unwrap();
        assert!(
            matches!(&fix.kind, StepKind::Agent { prompt, .. } if prompt.contains("${план.вывод}")),
            "{:?}", fix.kind
        );
    }

    #[test]
    fn a_file_that_is_not_bpmn_says_so() {
        assert!(from_xml("<html><body>привет</body></html>").is_err());
        assert!(from_xml("не xml вовсе").is_err());
    }

    /// Экранирование: промт с угловыми скобками и амперсандом обязан пережить
    /// путь в файл и обратно.
    #[test]
    fn markup_in_a_prompt_is_escaped_and_restored() {
        let p = Pipeline {
            start: "a".into(),
            steps: vec![agent("a", "почини <div> & \"кавычки\"", vec![Flow::to("", Cond::Always)])],
            ..Default::default()
        };
        let xml = to_xml(&p, "прогон");
        assert!(xml.contains("&lt;div&gt; &amp;"), "{xml}");
        let back = from_xml(&xml).unwrap();
        assert_eq!(
            back.steps[0].kind,
            StepKind::Agent { prompt: "почини <div> & \"кавычки\"".into(), model: String::new() }
        );
    }
}

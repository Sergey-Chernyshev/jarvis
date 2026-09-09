//! Остановка хода: Esc обязан дойти ДО ПРОЦЕССА, а не погасить индикатор.
//!
//! Хост живёт в своей задаче и читает stdout CLI — снаружи до него достаёт
//! только этот реестр: чат → ручка остановки. Ручка снимается сама (RAII, как
//! `TurnMark`): выходов из `run` десяток, а забытая запись означала бы «стоп» в
//! никуда.
//!
//! Почему группа процессов, а не брошенный future. Бросить чтение stdout —
//! худший исход из возможных: человек нажал «стоп», индикатор погас, а `claude`
//! остался сиротой и продолжил жечь токены уже без окна. `kill_on_drop` спасает
//! наполовину: он шлёт сигнал самому процессу, не ждёт его смерти и не знает про
//! его детей (`jarvis-mcp` и прочих). Поэтому хосты поднимают CLI ОТДЕЛЬНОЙ
//! группой (`process_group(0)`), а стоп бьёт SIGKILL по всей группе и дожидается
//! трупа — пока ядро не отдало статус, процесс жив.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufRead, Lines};
use tokio::sync::Notify;

use crate::capability::native::spawn::Spawn;

/// Потребитель гейта, от имени которого главный агент поднимает сессии.
const BY_AGENT: &str = "agent";

/// Ручка одного идущего хода: флаг «просили остановиться» и будильник для хоста.
#[derive(Default)]
struct Stop {
    asked: AtomicBool,
    wake: Notify,
}

impl Stop {
    /// Попросить остановиться — засчитывается РОВНО ОДИН раз. Второй Esc не
    /// заводит вторую остановку и не шлёт второго события в ленту.
    fn ask(&self) -> bool {
        let first = self
            .asked
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok();
        if first {
            // notify_one, а не notify_waiters: разрешение сохраняется, даже если
            // хост в этот миг разбирал строку и никого не ждал.
            self.wake.notify_one();
        }
        first
    }

    fn asked(&self) -> bool {
        self.asked.load(Ordering::SeqCst)
    }
}

fn registry() -> &'static Mutex<HashMap<String, Vec<Arc<Stop>>>> {
    static R: OnceLock<Mutex<HashMap<String, Vec<Arc<Stop>>>>> = OnceLock::new();
    R.get_or_init(Default::default)
}

/// Метка идущего хода: пока жива, Esc по этому чату доходит до процесса.
pub struct StopGate {
    chat_id: String,
    stop: Arc<Stop>,
}

impl StopGate {
    pub fn new(chat_id: &str) -> Self {
        let stop = Arc::new(Stop::default());
        if let Ok(mut r) = registry().lock() {
            // Список, а не один: один разговор могут вести два окна, и «стоп»
            // чата обязан погасить оба хода, а не первый попавшийся.
            r.entry(chat_id.trim().to_string())
                .or_default()
                .push(stop.clone());
        }
        StopGate { chat_id: chat_id.trim().to_string(), stop }
    }

    /// Ждать «стоп». Флаг проверяем ДО сна: просьба могла прийти, пока хост
    /// разбирал прошлую строку, — и тогда будить было некого.
    pub async fn wait(&self) {
        while !self.stop.asked() {
            self.stop.wake.notified().await;
        }
    }
}

impl Drop for StopGate {
    fn drop(&mut self) {
        let Ok(mut r) = registry().lock() else { return };
        let Some(list) = r.get_mut(&self.chat_id) else { return };
        list.retain(|s| !Arc::ptr_eq(s, &self.stop));
        if list.is_empty() {
            r.remove(&self.chat_id);
        }
    }
}

/// Что нашёл Esc в этом чате.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Ход шёл — остановлен.
    Stopped,
    /// Ход уже останавливается: второй Esc по тому же ходу.
    Already,
    /// Хода не было. Не ошибка: Esc нажали вхолостую, показывать нечего.
    Idle,
}

/// Попросить ход этого чата остановиться. Соседние чаты не задеваются: реестр
/// разведён по `chatId`, и просьба уходит только по своему ключу.
pub fn request(chat_id: &str) -> Outcome {
    let chat_id = chat_id.trim();
    let Ok(r) = registry().lock() else { return Outcome::Idle };
    let Some(list) = r.get(chat_id).filter(|l| !l.is_empty()) else {
        return Outcome::Idle;
    };
    let mut first = false;
    for s in list {
        first |= s.ask();
    }
    if first {
        Outcome::Stopped
    } else {
        Outcome::Already
    }
}

/// Что дальше делать с потоком CLI.
pub enum Next {
    Line(String),
    /// Поток кончился сам.
    End,
    /// Человек нажал «стоп».
    Stopped,
}

/// Следующая строка потока — или «остановлено».
///
/// `biased`: стоп важнее непрочитанного хвоста. Иначе Esc ждал бы, пока CLI
/// договорит, — а он в этот момент как раз и тратит деньги.
pub async fn next_line<R: AsyncBufRead + Unpin>(lines: &mut Lines<R>, gate: &StopGate) -> Next {
    tokio::select! {
        biased;
        () = gate.wait() => Next::Stopped,
        r = lines.next_line() => match r {
            Ok(Some(l)) => Next::Line(l),
            _ => Next::End,
        },
    }
}

/// Убить CLI насмерть вместе с его детьми и дождаться трупа.
///
/// Минус перед pid — это ГРУППА: хосты поднимают CLI отдельной группой, и без
/// этого дети (`jarvis-mcp`, поисковики) пережили бы родителя. `wait` не для
/// красоты: без него процесс остаётся зомби, а мы не знаем, умер ли он вообще.
pub async fn kill_tree(child: &mut tokio::process::Child) {
    let group = child.id().map(|pid| -(pid as libc::pid_t));
    if let Some(g) = group {
        unsafe {
            // SIGSTOP раньше SIGKILL — не перестраховка, а закрытая гонка: CLI
            // рождает детей непрерывно, и рождённый между сигналом и смертью
            // родителя в убитую группу уже не попадает. Замороженная — не родит.
            libc::kill(g, libc::SIGSTOP);
            libc::kill(g, libc::SIGKILL);
        }
    }
    // wait обязателен: без него процесс останется зомби, а мы не будем знать,
    // умер ли он вообще.
    let _ = child.kill().await;
    // Добиваем хвост. Родившийся ровно в миг первой волны в неё не попал, но и
    // рожать дальше уже некому — родителя нет, поэтому пары заходов хватает.
    // Проверено на заглушке: без добива сирота выживала в каждом третьем прогоне.
    let Some(g) = group else { return };
    for _ in 0..5 {
        if unsafe { libc::kill(g, 0) } != 0 {
            return; // группы больше нет — все умерли
        }
        unsafe { libc::kill(g, libc::SIGKILL) };
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    crate::log::line("[agent] группа процессов пережила стоп — проверь вручную");
}

/// Дочерняя сессия, пережившая остановку хода.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Child {
    /// id сессии, а пока её нет — талон запуска: снаружи это один и тот же запуск.
    pub id: String,
    pub name: String,
    pub agent: String,
}

/// Наш ли это запуск. Родителя пишет сам агент и делает это как придётся —
/// точного совпадения с id чата обычно нет. Поэтому чужим считаем ТОЛЬКО явно
/// названный соседний чат: спрятать работающую сессию хуже, чем назвать лишнюю.
/// Человек нажал «стоп» и должен увидеть, где ещё горят деньги.
fn ours(parent: Option<&str>, chat_id: &str, chats: &[String]) -> bool {
    match parent.map(str::trim).filter(|p| !p.is_empty()) {
        None => true,
        Some(p) if p == chat_id => true,
        Some(p) => !chats.iter().any(|c| c == p),
    }
}

/// Кого перечислить в «остановлено; сессии такие-то продолжают работу».
///
/// Дочерние сессии НЕ закрываются: там может идти ценная работа, за которую уже
/// заплачено. Отсюда и подпись — на входе срез реестра, а не сам реестр: тронуть
/// запуски эта функция не может по построению.
pub fn children_of(
    spawns: &[Spawn],
    chat_id: &str,
    chats: &[String],
    alive: impl Fn(&str) -> bool,
) -> Vec<Child> {
    spawns
        .iter()
        .filter(|s| s.by == BY_AGENT && !s.closed)
        .filter(|s| ours(s.parent.as_deref(), chat_id, chats))
        .filter(|s| match &s.session_id {
            Some(sid) => alive(sid),
            // Талон без сессии — она прямо сейчас поднимается: это тоже работа,
            // которая продолжится после нашего «стоп».
            None => true,
        })
        .map(|s| Child {
            id: s.session_id.clone().unwrap_or_else(|| s.ticket.clone()),
            name: s.name.clone(),
            agent: s.agent.clone(),
        })
        .collect()
}

/// Короткая строка для ленты. Говорит ровно то, что произошло: выдавать «хода не
/// было» за остановку нельзя — человек решит, что деньги перестали гореть.
pub fn note(outcome: Outcome, children: &[Child]) -> String {
    match outcome {
        Outcome::Idle => "Останавливать было нечего — ход не шёл".into(),
        Outcome::Already => "Ход уже останавливается".into(),
        Outcome::Stopped if children.is_empty() => "Остановлено вами".into(),
        Outcome::Stopped => format!(
            "Остановлено вами; продолжают работу: {}",
            children
                .iter()
                .map(|c| format!("{} ({})", c.name, c.agent))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

// ── Тесты ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tokio::io::AsyncBufReadExt;

    fn alive(pid: libc::pid_t) -> bool {
        unsafe { libc::kill(pid, 0) == 0 }
    }

    /// Заглушка вместо `claude`: печатает pid своего ребёнка и уходит спать.
    /// Поднимается ТОЧНО так же, как хосты, — отдельной группой.
    async fn stub() -> (tokio::process::Child, Lines<tokio::io::BufReader<tokio::process::ChildStdout>>, libc::pid_t) {
        use tokio::io::BufReader;
        let mut child = tokio::process::Command::new("sh")
            .arg("-c")
            .arg("sleep 60 & echo $!; sleep 60")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .process_group(0)
            .spawn()
            .expect("заглушка поднялась");
        let out = child.stdout.take().expect("stdout заглушки");
        let mut lines = BufReader::new(out).lines();
        let kid: libc::pid_t = lines
            .next_line()
            .await
            .ok()
            .flatten()
            .and_then(|l| l.trim().parse().ok())
            .expect("заглушка назвала pid ребёнка");
        (child, lines, kid)
    }

    /// Ждём смерти: SIGKILL доходит не мгновенно.
    async fn wait_dead(pid: libc::pid_t) -> bool {
        for _ in 0..100 {
            if !alive(pid) {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        false
    }

    /// Не осталось ли в группе НИКОГО — включая тех, о ком мы не знали. Именно
    /// это и значит «стоп дошёл до процесса»: два известных pid'а такого не
    /// доказывают, сирота обычно рождается третьей.
    async fn wait_group_dead(leader: libc::pid_t) -> bool {
        for _ in 0..100 {
            if unsafe { libc::kill(-leader, 0) } != 0 {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        false
    }

    /// Главная проверка владельца: «стоп» доходит до процесса, а не гасит
    /// индикатор. Первая половина теста показывает беду, ради которой всё и
    /// сделано, — убитый в одиночку CLI оставляет ребёнка сиротой, и тот
    /// продолжает работать (читай: жечь деньги) уже без окна.
    #[tokio::test]
    async fn stop_kills_the_whole_tree_not_just_the_parent() {
        // наивно: убить сам процесс — ребёнок переживает родителя
        let (mut naive, _lines, orphan) = stub().await;
        let group = naive.id().expect("pid заглушки") as libc::pid_t;
        let _ = naive.kill().await;
        assert!(alive(orphan), "вот она, сирота: ребёнок пережил смерть CLI");
        unsafe { libc::kill(-group, libc::SIGKILL) }; // прибираем за собой всю группу
        assert!(wait_dead(orphan).await, "тест не оставляет за собой живых заглушек");

        // как делаем мы: бьём по группе и дожидаемся трупа
        let (mut child, _lines, kid) = stub().await;
        let pid = child.id().expect("pid заглушки") as libc::pid_t;
        kill_tree(&mut child).await;
        assert!(wait_dead(pid).await, "процесс CLI пережил стоп — деньги горят дальше");
        assert!(wait_dead(kid).await, "ребёнок CLI осиротел и продолжил работу");
        assert!(
            wait_group_dead(pid).await,
            "в группе кто-то выжил: он родился в миг убийства и в первую волну не попал"
        );
    }

    /// Тот же путь, что у живого хода: стоп приходит СНАРУЖИ, через реестр, и
    /// поток бросает чтение немедленно — не дожидаясь, пока CLI договорит.
    #[tokio::test]
    async fn esc_from_outside_reaches_the_running_process() {
        let (mut child, mut lines, kid) = stub().await;
        let pid = child.id().expect("pid заглушки") as libc::pid_t;
        let gate = StopGate::new("c-live");

        assert_eq!(request("c-live"), Outcome::Stopped);
        let next = tokio::time::timeout(Duration::from_secs(5), next_line(&mut lines, &gate))
            .await
            .expect("поток обязан заметить стоп сразу, а не после конца вывода");
        assert!(matches!(next, Next::Stopped), "поток не увидел стоп");

        kill_tree(&mut child).await;
        assert!(wait_dead(pid).await);
        assert!(wait_dead(kid).await);
        assert!(wait_group_dead(pid).await, "после Esc в группе не должно остаться никого");
    }

    /// Пока ход идёт — есть кого останавливать; кончился — реестр пуст сам, без
    /// уборки на стороне вызывающего.
    #[tokio::test]
    async fn the_gate_registers_the_turn_and_clears_itself() {
        assert_eq!(request("c-raii"), Outcome::Idle, "хода не было — честное «нечего»");
        {
            let gate = StopGate::new("c-raii");
            assert_eq!(request("c-raii"), Outcome::Stopped);
            tokio::time::timeout(Duration::from_secs(2), gate.wait())
                .await
                .expect("хост обязан проснуться по стопу");
        }
        assert_eq!(request("c-raii"), Outcome::Idle, "ход кончился — метка снялась сама");
    }

    /// Двойной Esc не даёт второй «остановки» и ничего не роняет.
    #[tokio::test]
    async fn a_second_esc_is_not_a_second_stop() {
        let _gate = StopGate::new("c-twice");
        assert_eq!(request("c-twice"), Outcome::Stopped);
        assert_eq!(request("c-twice"), Outcome::Already, "второй Esc — не вторая остановка");
        assert_eq!(request("c-twice"), Outcome::Already);
        assert!(!note(Outcome::Already, &[]).contains("Остановлено вами"));
    }

    /// Потоки разведены по чатам: стоп одного разговора не задевает соседний.
    #[tokio::test]
    async fn stopping_one_chat_leaves_the_neighbour_alone() {
        let mine = StopGate::new("c1");
        let neighbour = StopGate::new("c2");
        assert_eq!(request("c1"), Outcome::Stopped);

        tokio::time::timeout(Duration::from_secs(2), mine.wait())
            .await
            .expect("свой ход обязан проснуться");
        assert!(
            tokio::time::timeout(Duration::from_millis(150), neighbour.wait())
                .await
                .is_err(),
            "соседний чат остановился заодно — этого делать нельзя"
        );
        assert_eq!(request("c2"), Outcome::Stopped, "сосед всё ещё останавливается своим Esc");
    }

    /// Один разговор в двух окнах — гасим оба хода, а не первый попавшийся.
    #[tokio::test]
    async fn two_turns_of_one_chat_are_both_stopped() {
        let a = StopGate::new("c-two");
        let b = StopGate::new("c-two");
        assert_eq!(request("c-two"), Outcome::Stopped);
        for (n, g) in [("первый", &a), ("второй", &b)] {
            tokio::time::timeout(Duration::from_secs(2), g.wait())
                .await
                .unwrap_or_else(|_| panic!("{n} ход не заметил стоп"));
        }
    }

    // ── дочерние сессии ───────────────────────────────────────────────────

    use crate::capability::native::spawn::{Plan, Spawns};

    fn plan(name: &str) -> Plan {
        Plan {
            agent: crate::backend::Agent::Claude,
            name: name.to_string(),
            task: "почини тесты".into(),
            cwd: "/tmp".into(),
            model: None,
        }
    }

    /// Дочерние сессии перечисляются — и остаются жить: там идёт работа, за
    /// которую заплачено. Реестр запусков после остановки нетронут.
    #[test]
    fn children_are_listed_and_stay_alive() {
        let s = Spawns::new();
        let mine = s.open("agent", Some("c1".into()), &plan("Сайдбар·JRV"), 0);
        s.bind(&mine, "sid-mine");
        let nameless = s.open("agent", None, &plan("Без родителя"), 0);
        s.bind(&nameless, "sid-nameless");
        let foreign = s.open("agent", Some("c2".into()), &plan("Чужой чат"), 0);
        s.bind(&foreign, "sid-foreign");
        let plugin = s.open("plugin:x", Some("c1".into()), &plan("Не наш"), 0);
        s.bind(&plugin, "sid-plugin");
        let dead = s.open("agent", Some("c1".into()), &plan("Уже умерла"), 0);
        s.bind(&dead, "sid-dead");
        let pending = s.open("agent", Some("c1".into()), &plan("Поднимается"), 0);

        let chats = vec!["c1".to_string(), "c2".to_string()];
        let kids = children_of(&s.snapshot(), "c1", &chats, |sid| sid != "sid-dead");
        let names: Vec<&str> = kids.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(
            names,
            ["Сайдбар·JRV", "Без родителя", "Поднимается"],
            "показываем свои и безродные живые запуски, чужой чат и чужого потребителя — нет"
        );
        assert_eq!(kids[0].id, "sid-mine");
        assert_eq!(kids[0].agent, "claude");
        assert_eq!(kids[2].id, pending, "у поднимающейся сессии id — талон запуска");

        // главное: перечислили — и не закрыли
        for t in [&mine, &nameless, &foreign, &plugin, &dead, &pending] {
            assert!(!s.find(t).expect("запись на месте").closed, "запуск закрыли: {t}");
        }
        // закрытые не показываем: работы там уже нет
        s.mark_closed(&mine);
        let kids = children_of(&s.snapshot(), "c1", &chats, |_| true);
        assert!(!kids.iter().any(|c| c.id == "sid-mine"));
    }

    /// Строка для ленты честна: «хода не было» не притворяется остановкой, а
    /// живые сессии названы поимённо.
    #[test]
    fn the_note_says_exactly_what_happened() {
        let kids = vec![
            Child { id: "s1".into(), name: "Сайдбар".into(), agent: "claude".into() },
            Child { id: "s2".into(), name: "Проверка".into(), agent: "kimi".into() },
        ];
        let t = note(Outcome::Stopped, &kids);
        assert!(t.contains("Сайдбар (claude)") && t.contains("Проверка (kimi)"), "{t}");
        assert!(t.contains("продолжают работу"), "человек должен знать, что горит дальше: {t}");
        assert_eq!(note(Outcome::Stopped, &[]), "Остановлено вами");
        let idle = note(Outcome::Idle, &kids);
        assert!(!idle.contains("Остановлено"), "вхолостую нажатый Esc не выдаём за стоп: {idle}");
    }

    // ── связки, которые нельзя потерять молча ─────────────────────────────

    /// Оба хоста обязаны пропускать свой поток через ручку и поднимать CLI
    /// отдельной группой: без первого Esc до процесса не дойдёт, без второго
    /// умрёт только сам CLI, а его дети осиротеют.
    #[test]
    fn both_hosts_are_wired_to_the_gate() {
        for (who, src) in [
            ("claude", include_str!("mod.rs")),
            ("codex", include_str!("../backend/codex_agent.rs")),
        ] {
            assert!(src.contains("StopGate::new"), "{who}: ход не в реестре — Esc до него не дойдёт");
            assert!(src.contains("Next::Stopped"), "{who}: поток не слушает остановку");
            assert!(src.contains("kill_tree"), "{who}: процесс переживёт стоп");
            assert!(src.contains("process_group(0)"), "{who}: CLI не в своей группе — дети осиротеют");
        }
    }

    /// Остановка рвёт и ЦЕПОЧКУ: иначе остановленный ход тут же сменится
    /// следующим, и карусель нечем прервать. Логику не дублируем — зовём ту же
    /// команду, что и кнопка «стоп цепочки»; тест сторожит именно вызов.
    #[test]
    fn stop_breaks_the_chain_through_the_existing_command() {
        let src = include_str!("../ipc.rs");
        let tail = src
            .split("pub fn agent_stop(")
            .nth(1)
            .expect("команда agent_stop на месте");
        let body = &tail[..tail.find("\n#[tauri::command]").unwrap_or(tail.len())];
        assert!(
            body.contains("agent_chain_stop("),
            "цепочку обязана рвать существующая команда, а не копия её логики"
        );
    }
}

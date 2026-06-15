//! Русские числительные и согласование существительных.
//!
//! Чистые функции без I/O — используются композитором фраз для TTS,
//! чтобы движок читал «два часа» вместо «2 часа».

/// Грамматический род — нужен для «один/одна», «два/две».
pub enum Gender {
    M,
    F,
}

/// Форма существительного по числу: (1, 2–4, 5+).
///
/// Стандартное русское правило:
/// - n % 100 в диапазоне 11–14 → форма «many»;
/// - иначе по последней цифре: 1 → one, 2–4 → few, иначе → many.
///
/// Пример: plural(2, "задача", "задачи", "задач") → "задачи".
pub fn plural<'a>(n: i64, one: &'a str, few: &'a str, many: &'a str) -> &'a str {
    let rem100 = n.abs() % 100;
    if (11..=14).contains(&rem100) {
        return many;
    }
    match rem100 % 10 {
        1 => one,
        2..=4 => few,
        _ => many,
    }
}

/// Число словами 0–999, мужской род.
pub fn number_words(n: i64) -> String {
    number_words_gender(n, Gender::M)
}

/// Число словами 0–999 с указанием рода (для «одна/две» женского рода).
pub fn number_words_gender(n: i64, g: Gender) -> String {
    debug_assert!((0..=999).contains(&n), "number_words_gender: n должен быть 0..=999");

    // Единицы: пары (мужской, женский)
    const UNITS: &[(&str, &str)] = &[
        ("ноль", "ноль"),
        ("один", "одна"),
        ("два", "две"),
        ("три", "три"),
        ("четыре", "четыре"),
        ("пять", "пять"),
        ("шесть", "шесть"),
        ("семь", "семь"),
        ("восемь", "восемь"),
        ("девять", "девять"),
    ];

    const TEENS: &[&str] = &[
        "десять",
        "одиннадцать",
        "двенадцать",
        "тринадцать",
        "четырнадцать",
        "пятнадцать",
        "шестнадцать",
        "семнадцать",
        "восемнадцать",
        "девятнадцать",
    ];

    const TENS: &[&str] = &[
        "",          // 0 (заглушка)
        "",          // 10 — покрыто TEENS[0]
        "двадцать",
        "тридцать",
        "сорок",
        "пятьдесят",
        "шестьдесят",
        "семьдесят",
        "восемьдесят",
        "девяносто",
    ];

    const HUNDREDS: &[&str] = &[
        "",
        "сто",
        "двести",
        "триста",
        "четыреста",
        "пятьсот",
        "шестьсот",
        "семьсот",
        "восемьсот",
        "девятьсот",
    ];

    let mut parts: Vec<&str> = Vec::new();

    // Сотни
    let h = (n / 100) as usize;
    if h > 0 {
        parts.push(HUNDREDS[h]);
    }

    let rem = n % 100;

    if (10..=19).contains(&rem) {
        // Тинейджеры — неизменяемые
        parts.push(TEENS[(rem - 10) as usize]);
    } else {
        // Десятки
        let t = (rem / 10) as usize;
        if t > 1 {
            parts.push(TENS[t]);
        }
        // Единицы
        let u = (rem % 10) as usize;
        if u > 0 || n == 0 {
            let word = match g {
                Gender::F => UNITS[u].1,
                Gender::M => UNITS[u].0,
            };
            parts.push(word);
        }
    }

    parts.join(" ")
}

/// Число словами в родительном падеже (для конструкции «из N задач»), диапазон 0–999.
///
/// Примеры: 6 → «шести», 3 → «трёх», 21 → «двадцати одного», 100 → «ста».
pub fn number_words_genitive(n: i64) -> String {
    debug_assert!((0..=999).contains(&n), "number_words_genitive: n должен быть 0..=999");

    // Сотни (родительный)
    const HUNDREDS_GEN: &[&str] = &[
        "",
        "ста",
        "двухсот",
        "трёхсот",
        "четырёхсот",
        "пятисот",
        "шестисот",
        "семисот",
        "восьмисот",
        "девятисот",
    ];
    // Десятки (родительный)
    const TENS_GEN: &[&str] = &[
        "", "", "двадцати", "тридцати", "сорока",
        "пятидесяти", "шестидесяти", "семидесяти", "восьмидесяти", "девяноста",
    ];
    // Тинейджеры (родительный)
    const TEENS_GEN: &[&str] = &[
        "десяти", "одиннадцати", "двенадцати", "тринадцати", "четырнадцати",
        "пятнадцати", "шестнадцати", "семнадцати", "восемнадцати", "девятнадцати",
    ];
    // Единицы (родительный, м.р.)
    const UNITS_GEN: &[&str] = &[
        "ноля", "одного", "двух", "трёх", "четырёх",
        "пяти", "шести", "семи", "восьми", "девяти",
    ];

    let mut parts: Vec<&str> = Vec::new();

    let h = (n / 100) as usize;
    if h > 0 {
        parts.push(HUNDREDS_GEN[h]);
    }

    let rem = n % 100;
    if (10..=19).contains(&rem) {
        parts.push(TEENS_GEN[(rem - 10) as usize]);
    } else {
        let t = (rem / 10) as usize;
        if t > 1 {
            parts.push(TENS_GEN[t]);
        }
        let u = (rem % 10) as usize;
        if u > 0 || n == 0 {
            parts.push(UNITS_GEN[u]);
        }
    }

    parts.join(" ")
}

/// «одна задача» / «две задачи» / «пять задач».
///
/// Число словами с родом + согласованная форма существительного.
pub fn count_phrase(n: i64, g: Gender, one: &str, few: &str, many: &str) -> String {
    format!("{} {}", number_words_gender(n, g), plural(n, one, few, many))
}

/// Длительность из минут словами.
///
/// Примеры: 5 → «пять минут», 60 → «один час», 120 → «два часа»,
/// 134 → «два часа четырнадцать минут».
pub fn duration_words(total_min: i64) -> String {
    let h = total_min / 60;
    let m = total_min % 60;

    let mut parts: Vec<String> = Vec::new();

    if h > 0 {
        parts.push(count_phrase(h, Gender::M, "час", "часа", "часов"));
    }
    if m > 0 || h == 0 {
        parts.push(count_phrase(m, Gender::F, "минута", "минуты", "минут"));
    }

    parts.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- plural ---

    #[test]
    fn plural_1() {
        assert_eq!(plural(1, "задача", "задачи", "задач"), "задача");
    }

    #[test]
    fn plural_2() {
        assert_eq!(plural(2, "задача", "задачи", "задач"), "задачи");
    }

    #[test]
    fn plural_5() {
        assert_eq!(plural(5, "задача", "задачи", "задач"), "задач");
    }

    #[test]
    fn plural_11_исключение() {
        // 11 — исключение: не «одна задача», а «задач»
        assert_eq!(plural(11, "задача", "задачи", "задач"), "задач");
    }

    #[test]
    fn plural_21() {
        assert_eq!(plural(21, "задача", "задачи", "задач"), "задача");
    }

    // --- number_words ---

    #[test]
    fn nw_0() {
        assert_eq!(number_words(0), "ноль");
    }

    #[test]
    fn nw_1() {
        assert_eq!(number_words(1), "один");
    }

    #[test]
    fn nw_2() {
        assert_eq!(number_words(2), "два");
    }

    #[test]
    fn nw_14() {
        assert_eq!(number_words(14), "четырнадцать");
    }

    #[test]
    fn nw_21() {
        assert_eq!(number_words(21), "двадцать один");
    }

    #[test]
    fn nw_100() {
        assert_eq!(number_words(100), "сто");
    }

    #[test]
    fn nw_246() {
        assert_eq!(number_words(246), "двести сорок шесть");
    }

    // --- count_phrase ---

    #[test]
    fn count_phrase_odna_zadacha() {
        // женский род: «одна задача»
        assert_eq!(
            count_phrase(1, Gender::F, "задача", "задачи", "задач"),
            "одна задача"
        );
    }

    #[test]
    fn count_phrase_tri_fayla() {
        // мужской род: «три файла»
        assert_eq!(
            count_phrase(3, Gender::M, "файл", "файла", "файлов"),
            "три файла"
        );
    }

    #[test]
    fn count_phrase_pyat_zadach() {
        assert_eq!(
            count_phrase(5, Gender::F, "задача", "задачи", "задач"),
            "пять задач"
        );
    }

    // --- number_words_genitive ---

    #[test]
    fn nwg_6() {
        assert_eq!(number_words_genitive(6), "шести");
    }

    #[test]
    fn nwg_3() {
        assert_eq!(number_words_genitive(3), "трёх");
    }

    #[test]
    fn nwg_21() {
        assert_eq!(number_words_genitive(21), "двадцати одного");
    }

    #[test]
    fn nwg_100() {
        assert_eq!(number_words_genitive(100), "ста");
    }

    // --- duration_words ---

    #[test]
    fn duration_5min() {
        assert_eq!(duration_words(5), "пять минут");
    }

    #[test]
    fn duration_60min() {
        assert_eq!(duration_words(60), "один час");
    }

    #[test]
    fn duration_120min() {
        assert_eq!(duration_words(120), "два часа");
    }

    #[test]
    fn duration_134min() {
        assert_eq!(duration_words(134), "два часа четырнадцать минут");
    }
}

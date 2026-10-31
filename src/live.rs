//! Answers that come from the transcript by themselves: when a line is a
//! question, or when someone says your name.
//!
//! Detection is a cheap heuristic, run on every transcript line. The model
//! makes the final call: the live prompt lets it reply `SKIP` when the line
//! does not need an answer from you, and such answers are dropped.

use std::sync::LazyLock;

use regex::Regex;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Trigger {
    /// The line reads as a question.
    Question,
    /// The line says one of your names.
    Mention,
}

impl Trigger {
    pub fn label(self) -> &'static str {
        match self {
            Trigger::Question => "question",
            Trigger::Mention => "your name",
        }
    }
}

/// Words that open a question, Indonesian and English.
const QUESTION_WORDS: &[&str] = &[
    // Indonesian
    "apa",
    "apakah",
    "siapa",
    "kapan",
    "dimana",
    "di mana",
    "kemana",
    "ke mana",
    "darimana",
    "dari mana",
    "mengapa",
    "kenapa",
    "bagaimana",
    "gimana",
    "berapa",
    "bisakah",
    "bisa",
    "boleh",
    "bolehkah",
    "mana",
    "yang mana",
    "adakah",
    "sudahkah",
    "sudah",
    "udah",
    "kok",
    "masa",
    "benarkah",
    "haruskah",
    "maukah",
    // English
    "what",
    "why",
    "how",
    "when",
    "where",
    "who",
    "whom",
    "whose",
    "which",
    "can",
    "could",
    "would",
    "will",
    "should",
    "shall",
    "do",
    "does",
    "did",
    "is",
    "are",
    "was",
    "were",
    "have",
    "has",
    "had",
    "may",
    "might",
    "any",
    "anyone",
    "isn't",
    "aren't",
    "don't",
    "doesn't",
    "didn't",
    "won't",
    "can't",
    "couldn't",
];

/// Phrases that ask for an opinion without a question word up front.
const ASKING_PHRASES: &[&str] = &[
    "menurut kamu",
    "menurut anda",
    "menurutmu",
    "gimana menurut",
    "bagaimana menurut",
    "ada ide",
    "ada saran",
    "ada pertanyaan",
    "tolong jelaskan",
    "bisa jelaskan",
    "what do you think",
    "any thoughts",
    "any ideas",
    "can you explain",
    "could you explain",
    "tell me",
    "thoughts on",
];

static WORDS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[\p{L}\p{N}']+").unwrap());

fn words(text: &str) -> Vec<String> {
    WORDS.find_iter(&text.to_lowercase()).map(|m| m.as_str().to_string()).collect()
}

/// Does this transcript line read as a question?
pub fn is_question(text: &str) -> bool {
    let text = text.trim();
    if text.contains('?') {
        return true;
    }
    let lower = text.to_lowercase();
    if ASKING_PHRASES.iter().any(|p| lower.contains(p)) {
        return true;
    }
    let w = words(&lower);
    // "Apa?" on its own is caught by the '?'; a two-word fragment without one
    // is usually not a question.
    if w.len() < 3 {
        return false;
    }
    let first_two = format!("{} {}", w[0], w[1]);
    QUESTION_WORDS.iter().any(|q| *q == w[0] || *q == first_two)
        || w.iter()
            .rev()
            .take(2)
            .any(|t| matches!(t.as_str(), "nggak" | "gak" | "enggak" | "kan" | "ya" | "belum" | "tidak"))
            && w.iter().any(|t| matches!(t.as_str(), "apa" | "apakah" | "sudah" | "udah" | "bisa" | "mau"))
}

fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for i in 1..=a.len() {
        let mut cur = vec![i; b.len() + 1];
        for j in 1..=b.len() {
            let cost = if a[i - 1] == b[j - 1] { 0 } else { 1 };
            cur[j] = (prev[j] + 1).min(cur[j - 1] + 1).min(prev[j - 1] + cost);
        }
        prev = cur;
    }
    prev[b.len()]
}

/// Does the line say one of `names`? Speech to text misspells names
/// ("Arief" for "Arif"), so names of four letters or more also match with
/// one letter changed or added.
pub fn mentions(text: &str, names: &[String]) -> bool {
    let w = words(text);
    names.iter().any(|name| {
        let name_words = words(name);
        if name_words.is_empty() {
            return false;
        }
        if name_words.len() > 1 {
            return text.to_lowercase().contains(&name.to_lowercase());
        }
        let n = &name_words[0];
        let len = n.chars().count();
        // One letter swapped or added ("Arip", "Arief") — not dropped, since
        // a shorter word is usually another name ("Ari").
        w.iter().any(|t| t == n || (len >= 4 && t.chars().count() >= len && edit_distance(t, n) <= 1))
    })
}

/// Which trigger, if any, fires for a new transcript line.
pub fn trigger(line: &str, auto_answer: bool, answer_on_name: bool, names: &[String]) -> Option<Trigger> {
    if answer_on_name && !names.is_empty() && mentions(line, names) {
        return Some(Trigger::Mention);
    }
    if auto_answer && is_question(line) {
        return Some(Trigger::Question);
    }
    None
}

/// What the model is asked when a line triggers a live answer.
pub fn prompt(recent: &[String], trigger: Trigger, names: &[String]) -> String {
    let me = if names.is_empty() { "the user".to_string() } else { names[0].clone() };
    let lines = recent.iter().map(|l| format!("- {l}")).collect::<Vec<_>>().join("\n");
    let why = match trigger {
        Trigger::Question => "The last line sounds like a question.".to_string(),
        Trigger::Mention => format!("The last line mentions {me}."),
    };
    format!(
        "You are listening to a live conversation on behalf of {me}. Transcript so far, oldest first \
         (speech to text, so expect misheard words):\n{lines}\n\n{why} If it asks {me} something, or is a \
         question {me} should be ready to answer, write what {me} can say right now: the answer first, in \
         the conversation's language, at most three short sentences, no preamble. If it does not need an \
         answer from {me}, reply with exactly SKIP."
    )
}

/// The model decided the line needs no answer.
pub fn is_skip(answer: &str) -> bool {
    let t = answer.trim().trim_matches(|c: char| c == '*' || c == '.' || c == '`' || c == '"');
    t.eq_ignore_ascii_case("skip")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn questions_in_both_languages() {
        for q in [
            "Kapan rilisnya?",
            "kapan rilis versi dua dijadwalkan",
            "Bagaimana kalau kita tunda ke minggu depan",
            "menurut kamu desain ini sudah oke",
            "Sudah dimigrasi belum databasenya",
            "What do you think about the new pricing",
            "Can you share the dashboard link",
            "is the API ready",
        ] {
            assert!(is_question(q), "{q}");
        }
        for s in [
            "Rilis versi dua dijadwalkan hari Jumat.",
            "Oke.",
            "Apa",
            "We shipped it yesterday.",
            "Saya sudah kirim emailnya tadi pagi.",
        ] {
            assert!(!is_question(s), "{s}");
        }
    }

    #[test]
    fn names_match_with_one_letter_off() {
        let names = vec!["Arif".to_string(), "Pak Siregar".to_string()];
        assert!(mentions("Arif, gimana menurutmu?", &names));
        assert!(mentions("arief bisa bantu?", &names), "whisper spelling");
        assert!(mentions("Arip, tolong cek", &names), "one letter changed");
        assert!(mentions("Tanya pak Siregar saja", &names));
        assert!(!mentions("Ari sudah datang", &names), "a different, shorter name");
        assert!(!mentions("arsitektur baru", &names));
        assert!(!mentions("anything", &[]));
    }

    #[test]
    fn trigger_respects_the_modes() {
        let names = vec!["Arif".to_string()];
        assert_eq!(trigger("Arif, sudah siap?", true, true, &names), Some(Trigger::Mention));
        assert_eq!(trigger("Arif tadi bilang oke.", false, true, &names), Some(Trigger::Mention));
        assert_eq!(trigger("Kapan rilisnya?", true, true, &names), Some(Trigger::Question));
        assert_eq!(trigger("Kapan rilisnya?", false, true, &names), None);
        assert_eq!(trigger("Arif, sudah siap?", true, false, &names), Some(Trigger::Question));
        assert_eq!(trigger("Oke kita lanjut.", true, true, &names), None);
    }

    #[test]
    fn prompt_and_skip() {
        let p = prompt(&["Kapan rilisnya?".into()], Trigger::Question, &["Arif".into()]);
        assert!(p.contains("on behalf of Arif") && p.contains("- Kapan rilisnya?") && p.contains("SKIP"));
        assert!(is_skip("SKIP") && is_skip(" skip. ") && is_skip("**SKIP**"));
        assert!(!is_skip("Skip the intro and start with the demo."));
    }
}

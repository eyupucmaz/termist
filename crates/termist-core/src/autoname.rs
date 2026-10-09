//! A card's name from the first prompt: its first few words that say something, as a
//! title. Local and plain: no model is asked.

/// Words a title is not made of.
const FILLER: &[&str] = &[
    "please", "can", "could", "would", "you", "the", "a", "an", "to", "i", "we", "let's", "lets",
    "just", "now", "also", "lütfen", "bir", "şu", "bu", "şimdi", "hadi", "bana", "da", "de", "mi",
    "ve", "ile",
];

/// Words in a name, at most.
pub const WORDS: usize = 5;
/// Characters in a name, at most.
pub const CHARS: usize = 40;

/// A name from `text`'s first line with something in it; `None` when no word is left.
pub fn from_prompt(text: &str) -> Option<String> {
    let line = text.lines().find(|l| !l.trim().is_empty())?;
    // Code between backticks says how, not what.
    let mut plain = String::with_capacity(line.len());
    let mut code = false;
    for c in line.chars() {
        match c {
            '`' => code = !code,
            _ if code => {}
            c => plain.push(c),
        }
    }
    let words: Vec<String> = plain
        .split_whitespace()
        // Links and paths are where, not what.
        .filter(|w| !w.contains('/') && !w.contains('\\'))
        .map(|w| w.trim_matches(|c: char| !c.is_alphanumeric()))
        .filter(|w| !w.is_empty() && !FILLER.contains(&w.to_lowercase().as_str()))
        .take(WORDS)
        .map(capital)
        .collect();
    let mut name = words.join(" ");
    let mut kept = words.len();
    while name.chars().count() > CHARS && kept > 1 {
        kept -= 1;
        name = words[..kept].join(" ");
    }
    if name.chars().count() > CHARS {
        name = name.chars().take(CHARS).collect();
    }
    (!name.is_empty()).then_some(name)
}

/// `word` with its first letter a capital.
fn capital(word: &str) -> String {
    let mut chars = word.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// A title an agent shows before it has a subject: its program's own name
/// (`✳ Claude Code`, `codex`).
pub fn generic_title(title: &str) -> bool {
    let words = title
        .trim_start_matches(|c: char| !c.is_alphanumeric())
        .trim()
        .to_lowercase();
    ["claude code", "claude", "codex", "opencode"].contains(&words.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_is_the_first_words_that_say_something() {
        assert_eq!(
            from_prompt("please fix the login redirect, it loses the query").as_deref(),
            Some("Fix Login Redirect It Loses")
        );
        assert_eq!(
            from_prompt("lütfen login sayfasındaki yönlendirmeyi düzelt").as_deref(),
            Some("Login Sayfasındaki Yönlendirmeyi Düzelt")
        );
        assert_eq!(
            from_prompt("\n\n  add tests\nfor the parser").as_deref(),
            Some("Add Tests"),
            "the first line with something in it"
        );
        assert_eq!(
            from_prompt(
                "look at https://github.com/acme/site/pull/2 and src/auth.rs, then `cargo test`"
            )
            .as_deref(),
            Some("Look At And Then"),
            "links, paths and code are left out"
        );
        assert_eq!(
            from_prompt("şu ıslak çöp kutusunu boşalt").as_deref(),
            Some("Islak Çöp Kutusunu Boşalt"),
            "Turkish letters stay"
        );
    }

    #[test]
    fn a_long_name_is_cut_at_a_word_and_nothing_left_is_no_name() {
        let long =
            from_prompt("internationalisation reconfiguration documentation verification").unwrap();
        assert!(long.chars().count() <= CHARS, "{long}");
        assert!(!long.ends_with(' '));
        assert_eq!(long, "Internationalisation Reconfiguration");
        assert_eq!(from_prompt("please, can you?"), None);
        assert_eq!(from_prompt("   "), None);
        assert_eq!(from_prompt("`only code`"), None);
    }

    #[test]
    fn a_program_s_own_name_is_no_title() {
        assert!(generic_title("✳ Claude Code"));
        assert!(generic_title("Claude Code"));
        assert!(generic_title("codex"));
        assert!(generic_title("OpenCode"));
        assert!(!generic_title("✳ Fix login redirect"));
        assert!(!generic_title("Claude Code review notes"));
    }
}

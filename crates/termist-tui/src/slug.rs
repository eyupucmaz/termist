//! A branch name from a task's prompt: `fix the login redirect` → `fix-the-login-redirect`;
//! from nothing, two words of Istanbul (`mavi-vapur`).

/// At most this many words of the prompt.
const WORDS: usize = 5;
/// At most this many characters.
const MAX: usize = 40;

const ADJECTIVES: &[&str] = &[
    "mavi", "sakin", "serin", "eski", "uzun", "ince", "hizli", "yesil", "tatli", "kisa", "derin",
    "sicak",
];
const NOUNS: &[&str] = &[
    "vapur", "marti", "simit", "kule", "iskele", "kopru", "kedi", "cay", "sokak", "bogaz", "ada",
    "ruzgar",
];

/// The branch name for `prompt`, not one of `taken` (a number goes after it if it is).
/// `seed` picks the two words when the prompt gives none.
pub fn branch(prompt: &str, taken: &[String], seed: u64) -> String {
    let base = from_prompt(prompt).unwrap_or_else(|| {
        let a = ADJECTIVES[(seed % ADJECTIVES.len() as u64) as usize];
        let n = NOUNS[((seed / ADJECTIVES.len() as u64) % NOUNS.len() as u64) as usize];
        format!("{a}-{n}")
    });
    if !taken.contains(&base) {
        return base;
    }
    (2..)
        .map(|i| format!("{base}-{i}"))
        .find(|b| !taken.iter().any(|t| t == b))
        .expect("a free name")
}

/// Lower case ASCII, Turkish letters made plain, anything else a `-` between words;
/// the first words, cut at `MAX`. `None` when nothing is left.
fn from_prompt(prompt: &str) -> Option<String> {
    let plain: String = prompt
        .chars()
        .flat_map(|c| match c {
            'ç' | 'Ç' => vec!['c'],
            'ğ' | 'Ğ' => vec!['g'],
            'ı' | 'I' | 'İ' | 'i' => vec!['i'],
            'ö' | 'Ö' => vec!['o'],
            'ş' | 'Ş' => vec!['s'],
            'ü' | 'Ü' => vec!['u'],
            c => c.to_lowercase().collect(),
        })
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let words: Vec<&str> = plain
        .split('-')
        .filter(|w| !w.is_empty())
        .take(WORDS)
        .collect();
    let mut out = String::new();
    for w in words {
        let next = if out.is_empty() {
            w.to_string()
        } else {
            format!("-{w}")
        };
        if out.len() + next.len() > MAX {
            if out.is_empty() {
                out = w.chars().take(MAX).collect();
            }
            break;
        }
        out.push_str(&next);
    }
    (!out.is_empty()).then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_prompt_gives_its_first_words_plain_and_joined() {
        assert_eq!(
            branch("Fix the login redirect!", &[], 0),
            "fix-the-login-redirect"
        );
        assert_eq!(
            branch(
                "Giriş yönlendirmesini düzelt, çıkışta şifre sorulmasın",
                &[],
                0
            ),
            "giris-yonlendirmesini-duzelt-cikista"
        );
        assert_eq!(branch("  a/b  c__d  ", &[], 0), "a-b-c-d");
        assert_eq!(
            branch(&"x".repeat(60), &[], 0),
            "x".repeat(40),
            "one long word is cut"
        );
        assert_eq!(
            branch(
                "refactor everything around the session store now please",
                &[],
                0
            ),
            "refactor-everything-around-the-session"
        );
    }

    #[test]
    fn nothing_to_go_on_gives_two_words_and_a_taken_name_a_number() {
        assert_eq!(branch("", &[], 0), "mavi-vapur");
        assert_eq!(branch("  …  ", &[], 13), "sakin-marti");
        let taken = vec!["fix-login".to_string(), "fix-login-2".to_string()];
        assert_eq!(branch("fix login", &taken, 0), "fix-login-3");
    }
}

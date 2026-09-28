//! What the host terminal can draw.
use termist_core::config::ColorDepth;

/// The colour depth the environment promises, never `Auto`. `var` reads an
/// environment variable (a parameter so tests need not touch the real environment).
pub fn color_depth(var: impl Fn(&str) -> Option<String>) -> ColorDepth {
    let colorterm = var("COLORTERM").unwrap_or_default().to_ascii_lowercase();
    if colorterm == "truecolor" || colorterm == "24bit" {
        return ColorDepth::TrueColor;
    }
    // Windows Terminal draws 24-bit colour but does not say so in COLORTERM.
    if cfg!(windows) && var("WT_SESSION").is_some() {
        return ColorDepth::TrueColor;
    }
    if var("TERM").is_some_and(|t| t.contains("256color")) {
        return ColorDepth::Ansi256;
    }
    ColorDepth::Ansi16
}

/// `setting`, or what the environment promises when it is `Auto`.
pub fn resolve_depth(setting: ColorDepth) -> ColorDepth {
    match setting {
        ColorDepth::Auto => color_depth(|k| std::env::var(k).ok()),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let pairs: Vec<(String, String)> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |k| {
            pairs
                .iter()
                .find(|(key, _)| key == k)
                .map(|(_, v)| v.clone())
        }
    }

    #[test]
    fn colorterm_promises_truecolor() {
        for value in ["truecolor", "24bit", "TrueColor"] {
            let depth = color_depth(env(&[("COLORTERM", value), ("TERM", "xterm")]));
            assert_eq!(depth, ColorDepth::TrueColor, "{value}");
        }
    }

    #[test]
    fn term_names_256_colours() {
        assert_eq!(
            color_depth(env(&[("TERM", "xterm-256color")])),
            ColorDepth::Ansi256
        );
        assert_eq!(
            color_depth(env(&[("TERM", "tmux-256color"), ("COLORTERM", "")])),
            ColorDepth::Ansi256
        );
    }

    #[test]
    fn nothing_known_is_16_colours() {
        assert_eq!(color_depth(env(&[])), ColorDepth::Ansi16);
        assert_eq!(color_depth(env(&[("TERM", "xterm")])), ColorDepth::Ansi16);
    }

    #[test]
    fn windows_terminal_is_truecolor_on_windows_only() {
        let depth = color_depth(env(&[("WT_SESSION", "x"), ("TERM", "xterm")]));
        let expected = if cfg!(windows) {
            ColorDepth::TrueColor
        } else {
            ColorDepth::Ansi16
        };
        assert_eq!(depth, expected);
    }

    #[test]
    fn a_setting_other_than_auto_is_kept() {
        assert_eq!(resolve_depth(ColorDepth::Ansi256), ColorDepth::Ansi256);
    }
}

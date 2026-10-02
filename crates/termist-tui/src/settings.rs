//! Changes the settings screen makes to config.toml. The file is edited in place: the
//! user's comments, blank lines and order stay as they were.
use toml_edit::{DocumentMut, Item, Table, value};

/// One change to config.toml.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConfigEdit {
    /// A text setting: `theme = "moda"`, or `notify.done_sound = "kedi"` in its table.
    Set { key: &'static str, value: String },
    /// A true/false setting: `animations = false`, `scenes.splash = false`.
    SetBool { key: &'static str, value: bool },
    /// A number: `scenes.idle_minutes = 5`.
    SetInt { key: &'static str, value: i64 },
    /// The whole of `[keys.<table>]`: the bindings that differ from the defaults.
    /// Empty removes the table.
    Keys {
        table: &'static str,
        bindings: Vec<(String, String)>,
    },
}

/// A new config.toml starts with a word on what it is.
const HEADER: &str = "\
# termist settings. The settings screen (s) writes here and keeps your comments.
# `termist config check` reports anything that is not used as written.
";

/// `text` (config.toml as it is, empty when there is none) with `edit` made.
pub fn apply(text: &str, edit: &ConfigEdit) -> Result<String, String> {
    let fresh = text.trim().is_empty();
    let mut doc: DocumentMut = text
        .parse()
        .map_err(|_| "config.toml is not valid TOML; fix it first (termist config check)")?;
    match edit {
        ConfigEdit::Set { key, value: v } => set(&mut doc, key, value(v.as_str()))?,
        ConfigEdit::SetBool { key, value: v } => set(&mut doc, key, value(*v))?,
        ConfigEdit::SetInt { key, value: v } => set(&mut doc, key, value(*v))?,
        ConfigEdit::Keys { table, bindings } => {
            if !doc.contains_key("keys") {
                let mut keys = Table::new();
                keys.set_implicit(true);
                doc.insert("keys", Item::Table(keys));
            }
            let keys = doc["keys"]
                .as_table_mut()
                .ok_or("keys in config.toml is not a table")?;
            if bindings.is_empty() {
                keys.remove(table);
            } else {
                if !keys.contains_key(table) {
                    keys.insert(table, Item::Table(Table::new()));
                }
                let t = keys[*table]
                    .as_table_mut()
                    .ok_or(format!("keys.{table} in config.toml is not a table"))?;
                let wanted: Vec<&str> = bindings.iter().map(|(k, _)| k.as_str()).collect();
                let stale: Vec<String> = t
                    .iter()
                    .map(|(k, _)| k.to_string())
                    .filter(|k| !wanted.contains(&k.as_str()))
                    .collect();
                for k in stale {
                    t.remove(&k);
                }
                for (k, action) in bindings {
                    match t.get_mut(k).and_then(|i| i.as_value_mut()) {
                        // Keeps a comment written after the value.
                        Some(existing) if existing.as_str() == Some(action) => {}
                        _ => {
                            t[k.as_str()] = value(action.as_str());
                        }
                    }
                }
            }
            if keys.is_empty() {
                doc.remove("keys");
            }
        }
    }
    Ok(if fresh {
        format!("{HEADER}\n{doc}")
    } else {
        doc.to_string()
    })
}

/// Puts `new` at the dotted `path`, making its table if there is none. A comment after
/// the old value stays after the new one.
fn set(doc: &mut DocumentMut, path: &str, new: Item) -> Result<(), String> {
    let mut parts: Vec<&str> = path.split('.').collect();
    let key = parts.pop().ok_or("an empty setting name")?;
    let mut table = doc.as_table_mut();
    for part in parts {
        if !table.contains_key(part) {
            table.insert(part, Item::Table(Table::new()));
        }
        table = table[part]
            .as_table_mut()
            .ok_or(format!("{part} in config.toml is not a table"))?;
    }
    let decor = table
        .get(key)
        .and_then(Item::as_value)
        .map(|old| old.decor().clone());
    table[key] = new;
    if let (Some(decor), Some(new)) = (decor, table[key].as_value_mut()) {
        *new.decor_mut() = decor;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use termist_core::config::Config;

    fn set(key: &'static str, v: &str) -> ConfigEdit {
        ConfigEdit::Set {
            key,
            value: v.into(),
        }
    }

    fn keys(table: &'static str, bindings: &[(&str, &str)]) -> ConfigEdit {
        ConfigEdit::Keys {
            table,
            bindings: bindings
                .iter()
                .map(|(k, a)| (k.to_string(), a.to_string()))
                .collect(),
        }
    }

    #[test]
    fn comments_blank_lines_and_order_are_kept() {
        let text = "# mine\nprefix = \"C-a\" # tmux-free\n\ntheme = \"uskudar\"   # the default\n\n[scenes]\n# quiet\nidle_minutes = 0\n";
        let out = apply(text, &set("theme", "moda")).unwrap();
        assert_eq!(
            out,
            "# mine\nprefix = \"C-a\" # tmux-free\n\ntheme = \"moda\"   # the default\n\n[scenes]\n# quiet\nidle_minutes = 0\n"
        );
    }

    #[test]
    fn a_new_setting_goes_before_the_tables_and_is_read_back() {
        let out = apply("[scenes]\nsplash = false\n", &set("colors", "256")).unwrap();
        let (config, problems) = Config::parse(&out, None);
        assert!(problems.is_empty(), "{problems:?}\n{out}");
        assert_eq!(config.colors, termist_core::config::ColorDepth::Ansi256);
        assert!(!config.scenes.splash);
        assert!(
            out.find("colors").unwrap() < out.find("[scenes]").unwrap(),
            "{out}"
        );
    }

    #[test]
    fn a_missing_file_starts_with_a_header() {
        let out = apply("", &set("theme", "moda")).unwrap();
        assert!(out.starts_with("# termist settings."), "{out}");
        assert!(out.ends_with("theme = \"moda\"\n"), "{out}");
    }

    #[test]
    fn key_bindings_are_replaced_as_a_whole_and_an_empty_table_goes() {
        let text = "theme = \"moda\"\n\n[keys.grid]\n\"p\" = \"none\" # I type p a lot\n\"x\" = \"quick_prompt\"\n";
        let out = apply(text, &keys("grid", &[("p", "none"), ("g", "quick_prompt")])).unwrap();
        assert!(out.contains("\"p\" = \"none\" # I type p a lot"), "{out}");
        assert!(out.contains("g = \"quick_prompt\""), "{out}");
        assert!(!out.contains("\"x\""), "{out}");
        let (config, problems) = Config::parse(&out, None);
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(config.keys.grid.len(), 2);

        let out = apply(&out, &keys("grid", &[])).unwrap();
        assert_eq!(out, "theme = \"moda\"\n");
        let out = apply(&out, &keys("focus", &[("z", "palette")])).unwrap();
        assert!(out.contains("[keys.focus]\nz = \"palette\""), "{out}");
    }

    #[test]
    fn keys_that_need_quotes_get_them() {
        let out = apply("", &keys("grid", &[("?", "help"), ("C-d", "none")])).unwrap();
        let (config, problems) = Config::parse(&out, None);
        assert!(problems.is_empty(), "{problems:?}\n{out}");
        assert_eq!(config.keys.grid["?"], "help");
        assert_eq!(config.keys.grid["C-d"], "none");
    }

    #[test]
    fn a_broken_file_is_not_touched() {
        let err = apply("theme = \"moda\n", &set("theme", "uskudar")).unwrap_err();
        assert!(err.contains("not valid TOML"), "{err}");
    }

    #[test]
    fn settings_in_tables_and_of_every_kind_are_written() {
        let text = "theme = \"moda\"\n\n[scenes]\n# mine\nsplash = true # at start\n";
        let out = apply(
            text,
            &ConfigEdit::SetBool {
                key: "scenes.splash",
                value: false,
            },
        )
        .unwrap();
        let out = apply(
            &out,
            &ConfigEdit::SetInt {
                key: "scenes.idle_minutes",
                value: 5,
            },
        )
        .unwrap();
        let out = apply(&out, &set("notify.waiting_sound", "bell")).unwrap();
        let out = apply(
            &out,
            &ConfigEdit::SetBool {
                key: "animations",
                value: false,
            },
        )
        .unwrap();
        assert!(
            out.contains("# mine\nsplash = false # at start\nidle_minutes = 5\n"),
            "{out}"
        );
        let (config, problems) = Config::parse(&out, None);
        assert!(problems.is_empty(), "{problems:?}\n{out}");
        assert!(!config.scenes.splash);
        assert_eq!(config.scenes.idle_minutes, 5);
        assert_eq!(
            config.notify.waiting_sound,
            termist_core::config::Sound::Bell
        );
        assert!(!config.animations);
    }
}

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
    /// `[[presets]]` (config.local.toml's are not among them): the presets termist knows (`known`, by name) replaced by `list`; any other
    /// `[[presets]]` table, one it could not read or never saw, stays as written. The
    /// list keeps its place in the file and the comment above it; none left removes it.
    Presets {
        list: Vec<termist_core::config::Preset>,
        known: Vec<String>,
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
        ConfigEdit::Presets { list, known } => {
            let old: Vec<Table> = match doc.remove("presets") {
                Some(Item::ArrayOfTables(a)) => a.into_iter().collect(),
                _ => vec![],
            };
            let lead = old
                .first()
                .and_then(|t| t.decor().prefix())
                .and_then(|p| p.as_str())
                .unwrap_or_default()
                .to_string();
            let at = old.first().and_then(Table::position);
            let named = |t: &Table| t.get("name").and_then(Item::as_str).map(str::to_string);
            let fresh_tables: Vec<Table> = list.iter().map(preset_table).collect();
            let mut all = toml_edit::ArrayOfTables::new();
            let mut placed = false;
            for t in old {
                let name = named(&t);
                let theirs = name.as_ref().is_some_and(|n| known.contains(n));
                let replaced = name
                    .as_ref()
                    .is_some_and(|n| list.iter().any(|p| &p.name == n));
                if theirs && !placed {
                    fresh_tables.iter().for_each(|f| all.push(f.clone()));
                    placed = true;
                } else if !theirs && !replaced {
                    all.push(t);
                }
            }
            if !placed {
                fresh_tables.into_iter().for_each(|f| all.push(f));
            }
            if all.is_empty() {
                if !lead.trim().is_empty() {
                    keep_comment(&mut doc, &lead, at.unwrap_or(isize::MAX));
                }
            } else {
                for (i, t) in all.iter_mut().enumerate() {
                    if at.is_some() {
                        t.set_position(at);
                    }
                    if i == 0 && at.is_some() {
                        t.decor_mut().set_prefix(lead.clone());
                    }
                }
                doc.insert("presets", Item::ArrayOfTables(all));
            }
        }
    }
    Ok(if fresh {
        format!("{HEADER}\n{doc}")
    } else {
        doc.to_string()
    })
}

/// A preset as a `[[presets]]` table, nothing written for what is not set.
fn preset_table(p: &termist_core::config::Preset) -> Table {
    let mut t = Table::new();
    t["name"] = value(p.name.as_str());
    t["harness"] = value(p.harness.id());
    for (key, v) in [
        ("model", p.model.as_deref().unwrap_or_default()),
        ("effort", p.effort.as_deref().unwrap_or_default()),
        ("prefix", p.prefix.as_str()),
        ("postfix", p.postfix.as_str()),
    ] {
        if !v.is_empty() {
            t[key] = value(v);
        }
    }
    t
}

/// The comment that stood above a removed table, kept above the table that came next,
/// or at the end of the file when none did.
fn keep_comment(doc: &mut DocumentMut, comment: &str, after: isize) {
    fn next(t: &Table, after: isize, best: &mut Option<isize>) {
        for (_, item) in t.iter() {
            let tables: Vec<&Table> = match item {
                Item::Table(t) => vec![t],
                Item::ArrayOfTables(a) => a.iter().collect(),
                _ => vec![],
            };
            for t in tables {
                if let Some(p) = t.position().filter(|&p| p > after && !t.is_implicit()) {
                    *best = Some(best.map_or(p, |b| b.min(p)));
                }
                next(t, after, best);
            }
        }
    }
    fn prepend(t: &mut Table, at: isize, comment: &str) -> bool {
        for (_, item) in t.iter_mut() {
            let tables: Vec<&mut Table> = match item {
                Item::Table(t) => vec![t],
                Item::ArrayOfTables(a) => a.iter_mut().collect(),
                _ => vec![],
            };
            for t in tables {
                if t.position() == Some(at) && !t.is_implicit() {
                    let was = t
                        .decor()
                        .prefix()
                        .and_then(|p| p.as_str())
                        .unwrap_or_default();
                    let now = format!("{comment}{}", was.trim_start_matches('\n'));
                    t.decor_mut().set_prefix(now);
                    return true;
                }
                if prepend(t, at, comment) {
                    return true;
                }
            }
        }
        false
    }
    let mut best = None;
    next(doc.as_table(), after, &mut best);
    if !best.is_some_and(|at| prepend(doc.as_table_mut(), at, comment)) {
        let trailing = doc.trailing().as_str().unwrap_or_default().to_string();
        doc.set_trailing(format!("{comment}{trailing}"));
    }
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

    fn preset(name: &str, prefix: &str, postfix: &str) -> termist_core::config::Preset {
        termist_core::config::Preset {
            name: name.into(),
            harness: termist_core::Harness::Claude,
            model: Some("opus".into()),
            effort: None,
            prefix: prefix.into(),
            postfix: postfix.into(),
            local: false,
        }
    }

    #[test]
    fn presets_are_written_as_a_list_and_read_back_the_same() {
        let text = "# mine\ntheme = \"moda\" # dark\n";
        let list = vec![
            preset("review", "Review this: ", "\nThen list what to fix."),
            preset("plain", "", ""),
        ];
        let out = apply(
            text,
            &ConfigEdit::Presets {
                list: list.clone(),
                known: vec![],
            },
        )
        .unwrap();
        assert!(
            out.starts_with("# mine\ntheme = \"moda\" # dark\n"),
            "{out}"
        );
        assert!(
            !out.contains("effort"),
            "nothing for what is not set: {out}"
        );
        let (read, problems) = Config::parse(&out, None);
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(read.presets, list);
        // Written again: the list is replaced, not added to; none removes it.
        let out = apply(
            &out,
            &ConfigEdit::Presets {
                list: vec![list[1].clone()],
                known: vec!["review".into(), "plain".into()],
            },
        )
        .unwrap();
        assert_eq!(Config::parse(&out, None).0.presets, [list[1].clone()]);
        let out = apply(
            &out,
            &ConfigEdit::Presets {
                list: vec![],
                known: vec!["plain".into()],
            },
        )
        .unwrap();
        assert!(!out.contains("presets"), "{out}");
        assert!(out.contains("theme = \"moda\" # dark"));
    }

    #[test]
    fn presets_keep_their_place_their_comment_and_what_termist_did_not_know() {
        let text = "# mine\n\n[[presets]]\nname = \"review\"\nharness = \"claude\"\n\n[[presets]]\nname = \"deploy\"\nharness = \"clade\"\n\n[scenes]\nsplash = false\n";
        let edit = |list: Vec<termist_core::config::Preset>, known: &[&str]| ConfigEdit::Presets {
            list,
            known: known.iter().map(|n| n.to_string()).collect(),
        };
        let out = apply(text, &edit(vec![preset("careful", "", "")], &["review"])).unwrap();
        assert!(
            out.starts_with("# mine\n\n[[presets]]\nname = \"careful\""),
            "{out}"
        );
        // A preset termist never showed (here a broken one) stays as written.
        assert!(
            out.contains("name = \"deploy\"\nharness = \"clade\""),
            "{out}"
        );
        assert!(out.find("deploy") < out.find("[scenes]"), "{out}");
        let out = apply(&out, &edit(vec![], &["careful"])).unwrap();
        assert!(out.starts_with("# mine\n"), "{out}");
        assert!(!out.contains("careful") && out.contains("deploy"), "{out}");
    }

    #[test]
    fn the_last_preset_removed_leaves_the_comment_above_it() {
        let edit = ConfigEdit::Presets {
            list: vec![],
            known: vec!["a".to_string()],
        };
        let alone = "# mine\n\n[[presets]]\nname = \"a\"\nharness = \"claude\"\n";
        let out = apply(alone, &edit).unwrap();
        assert!(out.contains("# mine") && !out.contains("presets"), "{out}");
        let before = "# mine\n\n[[presets]]\nname = \"a\"\nharness = \"claude\"\n\n[scenes]\nsplash = false\n";
        let out = apply(before, &edit).unwrap();
        assert!(out.starts_with("# mine\n"), "{out}");
        assert!(out.contains("[scenes]\nsplash = false"), "{out}");
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

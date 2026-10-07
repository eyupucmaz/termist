//! The words that hand a pull request's review threads to an agent: where each one is,
//! what was said, and the lines it is about.
use super::timeline::hunk_tail;
use termist_core::github::Thread;

/// `Review comments on #212 (fix/login):` then each thread, numbered.
pub fn text(number: u32, branch: &str, threads: &[&Thread]) -> String {
    let mut out = format!("Review comments on #{number} ({branch}):\n");
    for (i, th) in threads.iter().enumerate() {
        out.push('\n');
        let at = match (th.start_line, th.line) {
            (Some(start), Some(line)) => format!("{}:{start}-{line}", th.path),
            (None, Some(line)) => format!("{}:{line}", th.path),
            _ => format!("{} (outdated)", th.path),
        };
        out.push_str(&format!("{}. {at}\n", i + 1));
        for c in &th.comments {
            let mut lines = c.body.trim().lines();
            out.push_str(&format!(
                "   {}: {}\n",
                c.author,
                lines.next().unwrap_or("")
            ));
            for line in lines {
                out.push_str(&format!("      {line}\n"));
            }
        }
        let context = hunk_tail(&th.hunk, 3);
        if !context.is_empty() {
            out.push_str("   ```diff\n");
            for (_, mark, text) in context {
                out.push_str(&format!("   {mark}{text}\n"));
            }
            out.push_str("   ```\n");
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use termist_core::github::{Comment, Side};

    fn thread(path: &str, start: Option<u32>, line: Option<u32>, said: &[(&str, &str)]) -> Thread {
        Thread {
            id: format!("T_{path}"),
            path: path.into(),
            line,
            start_line: start,
            side: Side::Right,
            resolved: false,
            outdated: line.is_none(),
            hunk: "@@ -38,3 +38,4 @@ fn go()\n let a = 1;\n-  go(url)\n+  go(url, query)\n".into(),
            comments: said
                .iter()
                .map(|(author, body)| Comment {
                    id: "C".into(),
                    author: author.to_string(),
                    body: body.to_string(),
                    created_at: "2026-10-06T10:00:00Z".into(),
                    mine: false,
                    can_edit: false,
                    can_delete: false,
                    pending: false,
                })
                .collect(),
            more: 0,
            can_reply: true,
            can_resolve: true,
        }
    }

    #[test]
    fn each_thread_says_where_who_said_what_and_the_lines_it_is_about() {
        let a = thread(
            "src/App.tsx",
            Some(40),
            Some(42),
            &[
                ("alice", "the redirect drops the query string"),
                ("bob", "agreed, use location.search\nand keep the hash"),
            ],
        );
        let mut b = thread(
            "README.md",
            None,
            Some(12),
            &[("carol", "typo: \"recieve\"")],
        );
        b.hunk = String::new();
        let gone = thread("old.rs", None, None, &[("dan", "why?")]);
        assert_eq!(
            text(212, "fix/login", &[&a, &b, &gone]),
            "Review comments on #212 (fix/login):\n\
             \n\
             1. src/App.tsx:40-42\n   \
             alice: the redirect drops the query string\n   \
             bob: agreed, use location.search\n      \
             and keep the hash\n   \
             ```diff\n    \
             let a = 1;\n   \
             -  go(url)\n   \
             +  go(url, query)\n   \
             ```\n\
             \n\
             2. README.md:12\n   \
             carol: typo: \"recieve\"\n\
             \n\
             3. old.rs (outdated)\n   \
             dan: why?\n   \
             ```diff\n    \
             let a = 1;\n   \
             -  go(url)\n   \
             +  go(url, query)\n   \
             ```\n"
        );
    }
}

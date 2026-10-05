//! A pull request's diff: its files from GitHub's REST API, each with its patch, and
//! whether you viewed each one, from GraphQL. Marking a file viewed is here too.
use super::gh::{self, Gh};
use super::query::{quoted, viewed};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use termist_core::github::{DiffFile, GhState, Patch, PrDiff, Viewed};

/// Pages of files read, 100 each: GitHub's own page size.
pub const PAGES: u32 = 3;
pub const PER_PAGE: u32 = 100;
/// Patches past this many bytes, all files together, are not kept.
pub const MAX_BYTES: usize = 8 * 1024 * 1024;

/// A file as the REST API lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RestFile {
    pub path: String,
    pub previous: Option<String>,
    /// `added`, `removed`, `modified`, `renamed`, `copied`, `changed`, `unchanged`.
    pub status: String,
    pub additions: u32,
    pub deletions: u32,
    pub changes: u32,
    pub patch: Option<String>,
}

pub fn page_path(owner: &str, name: &str, number: u32, page: u32) -> String {
    format!("repos/{owner}/{name}/pulls/{number}/files?per_page={PER_PAGE}&page={page}")
}

/// One page of the REST answer: a list of files.
pub fn parse_page(v: &Value) -> Result<Vec<RestFile>, GhState> {
    let list = v
        .as_array()
        .ok_or_else(|| GhState::Failed("GitHub sent no list of files".into()))?;
    let n = |f: &Value, key: &str| f[key].as_u64().unwrap_or(0) as u32;
    Ok(list
        .iter()
        .filter_map(|f| {
            Some(RestFile {
                path: f["filename"].as_str()?.to_string(),
                previous: f["previous_filename"].as_str().map(str::to_string),
                status: f["status"].as_str().unwrap_or("modified").to_string(),
                additions: n(f, "additions"),
                deletions: n(f, "deletions"),
                changes: n(f, "changes"),
                patch: f["patch"].as_str().map(str::to_string),
            })
        })
        .collect())
}

/// Whether you viewed each file, a page of 100 at a time.
pub fn viewed_query(owner: &str, name: &str, number: u32, after: Option<&str>) -> String {
    let after = after
        .map(|c| format!(", after: {}", quoted(c)))
        .unwrap_or_default();
    format!(
        "query {{ repository(owner: {}, name: {}) {{ pullRequest(number: {number}) {{
  files(first: {PER_PAGE}{after}) {{ pageInfo {{ hasNextPage endCursor }} nodes {{ path viewerViewedState }} }}
}} }} }}",
        quoted(owner),
        quoted(name)
    )
}

/// The viewed states of one page, and the cursor of the next page if there is one.
pub fn parse_viewed(v: &Value) -> (Vec<(String, Viewed)>, Option<String>) {
    let files = &v["data"]["repository"]["pullRequest"]["files"];
    let states = files["nodes"]
        .as_array()
        .map(|nodes| {
            nodes
                .iter()
                .filter_map(|f| {
                    Some((
                        f["path"].as_str()?.to_string(),
                        viewed(&f["viewerViewedState"]),
                    ))
                })
                .collect()
        })
        .unwrap_or_default();
    let next = match files["pageInfo"]["hasNextPage"].as_bool() {
        Some(true) => files["pageInfo"]["endCursor"].as_str().map(str::to_string),
        _ => None,
    };
    (states, next)
}

/// The anchor of a file on GitHub's "Files changed" page: the SHA-256 of its path.
pub fn anchor(path: &str) -> String {
    Sha256::digest(path.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// The REST files as termist's diff. `changed` is the PR's count of changed files.
pub fn assemble(
    files: Vec<RestFile>,
    viewed: &HashMap<String, Viewed>,
    pr_url: &str,
    changed: u32,
    head_oid: &str,
) -> PrDiff {
    let mut bytes = 0usize;
    let read = files.len() as u32;
    let files = files
        .into_iter()
        .map(|f| {
            let patch = match f.patch {
                Some(text) if bytes + text.len() <= MAX_BYTES => {
                    bytes += text.len();
                    Patch::Text(text)
                }
                Some(_) => Patch::TooLarge,
                None if f.changes == 0 && f.status == "renamed" => Patch::Renamed,
                None if f.changes == 0 => Patch::Binary,
                None => Patch::TooLarge,
            };
            let change = match f.status.as_str() {
                "added" => 'A',
                "removed" => 'D',
                "renamed" => 'R',
                "copied" => 'C',
                _ => 'M',
            };
            DiffFile {
                url: format!("{pr_url}/files#diff-{}", anchor(&f.path)),
                viewed: viewed.get(&f.path).copied().unwrap_or_default(),
                path: f.path,
                previous: f.previous,
                change,
                additions: f.additions,
                deletions: f.deletions,
                patch,
            }
        })
        .collect();
    PrDiff {
        head_oid: head_oid.to_string(),
        files,
        more: changed.saturating_sub(read),
    }
}

/// What a diff job needs to know of the pull request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Want {
    pub owner: String,
    pub name: String,
    pub number: u32,
    pub url: String,
    pub changed: u32,
    pub head_oid: String,
}

/// Reads the whole diff as `token`: up to `PAGES` pages of files, then their viewed
/// states. Any call that fails fails the whole read.
pub fn fetch(gh: &dyn Gh, token: &str, want: &Want) -> Result<PrDiff, GhState> {
    let mut files = Vec::new();
    for page in 1..=PAGES {
        let v = gh::rest(
            gh,
            token,
            &page_path(&want.owner, &want.name, want.number, page),
        )?;
        let got = parse_page(&v)?;
        let full = got.len() as u32 == PER_PAGE;
        files.extend(got);
        if !full {
            break;
        }
    }
    let mut states = HashMap::new();
    let mut after: Option<String> = None;
    for _ in 0..PAGES {
        let q = viewed_query(&want.owner, &want.name, want.number, after.as_deref());
        let (page, next) = parse_viewed(&gh::graphql(gh, token, &q)?);
        states.extend(page);
        match next {
            Some(cursor) => after = Some(cursor),
            None => break,
        }
    }
    Ok(assemble(
        files,
        &states,
        &want.url,
        want.changed,
        &want.head_oid,
    ))
}

/// Marks `path` of pull request `id` viewed (or not) on GitHub.
pub fn mark_query(id: &str, path: &str, viewed: bool) -> String {
    let field = if viewed {
        "markFileAsViewed"
    } else {
        "unmarkFileAsViewed"
    };
    format!(
        "mutation {{ {field}(input: {{pullRequestId: {}, path: {}}}) {{ clientMutationId }} }}",
        quoted(id),
        quoted(path)
    )
}

/// A mutation's answer: GitHub says no with the data still there and the field null.
pub fn parse_marked(v: &Value, viewed: bool) -> Result<(), GhState> {
    let field = if viewed {
        "markFileAsViewed"
    } else {
        "unmarkFileAsViewed"
    };
    if !v["data"][field].is_null() {
        return Ok(());
    }
    let why = v["errors"][0]["message"]
        .as_str()
        .unwrap_or("GitHub did not take it");
    Err(GhState::Failed(why.to_string()))
}

pub fn mark(gh: &dyn Gh, token: &str, id: &str, path: &str, viewed: bool) -> Result<(), GhState> {
    let v = gh::graphql(gh, token, &mark_query(id, path, viewed))?;
    parse_marked(&v, viewed)
}

#[cfg(test)]
pub mod tests {
    use super::super::gh::fake::{FakeGh, fails, ok};
    use super::*;

    fn rest_file(path: &str, status: &str, changes: u32, patch: Option<&str>) -> RestFile {
        RestFile {
            path: path.into(),
            previous: None,
            status: status.into(),
            additions: changes,
            deletions: 0,
            changes,
            patch: patch.map(str::to_string),
        }
    }

    fn json(s: &str) -> Value {
        serde_json::from_str(s).unwrap()
    }

    /// Synthetic: a modified file, a rename and a binary file.
    pub const PAGE: &str = r#"[
      {"sha":"1","filename":"src/search/DealerFilter.tsx","status":"modified","additions":2,"deletions":1,"changes":3,
       "patch":"@@ -38,3 +38,4 @@ export function DealerFilter\n   const dealers = useDealers();\n-  const label = 'All';\n+  const label = sel ?? 'All';\n+  useEffect(() => fetchAll(), []);"},
      {"sha":"2","filename":"src/api/client.ts","previous_filename":"src/client.ts","status":"renamed","additions":0,"deletions":0,"changes":0},
      {"sha":"3","filename":"public/logo.png","status":"added","additions":0,"deletions":0,"changes":0}
    ]"#;

    pub const VIEWED: &str = r#"{"data":{"repository":{"pullRequest":{"files":{
      "pageInfo":{"hasNextPage":false,"endCursor":"Mw"},
      "nodes":[{"path":"src/search/DealerFilter.tsx","viewerViewedState":"VIEWED"},
               {"path":"public/logo.png","viewerViewedState":"DISMISSED"}]}}}}}"#;

    #[test]
    fn a_page_of_files_is_read() {
        let files = parse_page(&json(PAGE)).unwrap();
        assert_eq!(files.len(), 3);
        assert_eq!(files[1].previous.as_deref(), Some("src/client.ts"));
        assert!(files[0].patch.as_deref().unwrap().starts_with("@@ -38,3"));
        assert_eq!(files[2].patch, None);
        assert!(parse_page(&json(r#"{"message":"x"}"#)).is_err());
    }

    #[test]
    fn files_without_a_patch_say_why() {
        let d = assemble(
            vec![
                rest_file("a.rs", "modified", 3, Some("@@ -1 +1 @@\n-a\n+b")),
                rest_file("b.rs", "renamed", 0, None),
                rest_file("c.png", "added", 0, None),
                rest_file("d.lock", "modified", 9000, None),
            ],
            &HashMap::new(),
            "https://github.com/acme/site/pull/7",
            4,
            "h1",
        );
        let patches: Vec<&Patch> = d.files.iter().map(|f| &f.patch).collect();
        assert!(matches!(patches[0], Patch::Text(_)));
        assert_eq!(
            patches[1..],
            [&Patch::Renamed, &Patch::Binary, &Patch::TooLarge]
        );
        let changes: String = d.files.iter().map(|f| f.change).collect();
        assert_eq!(changes, "MRAM");
        assert_eq!((d.head_oid.as_str(), d.more), ("h1", 0));
    }

    #[test]
    fn patches_past_the_limit_are_too_large() {
        let big = "x".repeat(MAX_BYTES / 2 + 1);
        let d = assemble(
            vec![
                rest_file("a", "modified", 1, Some(&big)),
                rest_file("b", "modified", 1, Some(&big)),
                rest_file("c", "modified", 1, Some("small")),
            ],
            &HashMap::new(),
            "u",
            10,
            "h",
        );
        assert!(matches!(d.files[0].patch, Patch::Text(_)));
        assert_eq!(d.files[1].patch, Patch::TooLarge);
        assert!(
            matches!(d.files[2].patch, Patch::Text(_)),
            "a small one still fits"
        );
        assert_eq!(d.more, 7);
    }

    #[test]
    fn each_file_links_to_its_place_on_github() {
        assert_eq!(
            anchor("a"),
            "ca978112ca1bbdcafac231b39a23dc4da786eff8147c4e72b9807785afee48bb"
        );
        let d = assemble(
            vec![rest_file("a", "modified", 1, Some("p"))],
            &HashMap::new(),
            "https://github.com/acme/site/pull/7",
            1,
            "h",
        );
        assert_eq!(
            d.files[0].url,
            format!(
                "https://github.com/acme/site/pull/7/files#diff-{}",
                anchor("a")
            )
        );
    }

    #[test]
    fn viewed_states_page_through_their_cursor() {
        let (states, next) = parse_viewed(&json(VIEWED));
        assert_eq!(
            states,
            [
                ("src/search/DealerFilter.tsx".to_string(), Viewed::Viewed),
                ("public/logo.png".to_string(), Viewed::Dismissed)
            ]
        );
        assert_eq!(next, None);
        let more = VIEWED.replace(r#""hasNextPage":false"#, r#""hasNextPage":true"#);
        assert_eq!(parse_viewed(&json(&more)).1.as_deref(), Some("Mw"));
        let q = viewed_query("acme", "site", 7, Some("Mw"));
        assert!(q.contains(r#"files(first: 100, after: "Mw")"#), "{q}");
        assert!(viewed_query("acme", "site", 7, None).contains("files(first: 100)"));
    }

    fn want() -> Want {
        Want {
            owner: "acme".into(),
            name: "site".into(),
            number: 7,
            url: "https://github.com/acme/site/pull/7".into(),
            changed: 3,
            head_oid: "h1".into(),
        }
    }

    #[test]
    fn fetch_reads_the_files_then_their_viewed_states() {
        let fake = FakeGh::new(|c| {
            if c.args.contains(&"graphql".to_string()) {
                ok(VIEWED)
            } else {
                ok(PAGE)
            }
        });
        let d = fetch(&*fake, "tok", &want()).unwrap();
        assert_eq!(d.files.len(), 3);
        assert_eq!(d.files[0].viewed, Viewed::Viewed);
        assert_eq!(d.files[1].viewed, Viewed::Unviewed);
        assert_eq!(d.files[2].viewed, Viewed::Dismissed);
        let calls = fake.calls();
        assert_eq!(calls.len(), 2, "a short page is the last one");
        assert_eq!(
            calls[0].args[3],
            "repos/acme/site/pulls/7/files?per_page=100&page=1"
        );
    }

    #[test]
    fn fetch_stops_after_three_pages() {
        let full: Vec<String> = (0..100)
            .map(|i| format!(r#"{{"filename":"f{i}","status":"added","changes":1,"patch":"@@ -0,0 +1 @@\n+x"}}"#))
            .collect();
        let full = format!("[{}]", full.join(","));
        let fake = FakeGh::new(move |c| {
            if c.args.contains(&"graphql".to_string()) {
                ok(VIEWED)
            } else {
                ok(&full)
            }
        });
        let mut w = want();
        w.changed = 450;
        let d = fetch(&*fake, "tok", &w).unwrap();
        assert_eq!(d.files.len(), 300);
        assert_eq!(d.more, 150);
        let rest = fake
            .calls()
            .iter()
            .filter(|c| !c.args.contains(&"graphql".to_string()))
            .count();
        assert_eq!(rest, 3);
    }

    #[test]
    fn a_failing_call_fails_the_read() {
        let fake = FakeGh::new(|c| {
            if c.args.contains(&"graphql".to_string()) {
                fails("", "gh: API rate limit exceeded")
            } else {
                ok(PAGE)
            }
        });
        assert!(matches!(
            fetch(&*fake, "tok", &want()),
            Err(GhState::RateLimited { .. })
        ));
    }

    #[test]
    fn marking_viewed_quotes_the_path_and_reads_a_refusal() {
        let q = mark_query("PR_1", r#"src/a "b".rs"#, true);
        assert!(
            q.starts_with("mutation { markFileAsViewed(input: {pullRequestId: \"PR_1\""),
            "{q}"
        );
        assert!(q.contains(r#"path: "src/a \"b\".rs""#), "{q}");
        assert!(mark_query("PR_1", "a", false).contains("unmarkFileAsViewed"));
        let yes = json(r#"{"data":{"markFileAsViewed":{"clientMutationId":null}}}"#);
        assert_eq!(parse_marked(&yes, true), Ok(()));
        let no = json(
            r#"{"data":{"markFileAsViewed":null},"errors":[{"type":"NOT_FOUND","message":"Could not resolve to a node"}]}"#,
        );
        assert_eq!(
            parse_marked(&no, true),
            Err(GhState::Failed("Could not resolve to a node".into()))
        );
        let fake =
            FakeGh::new(|_| ok(r#"{"data":{"unmarkFileAsViewed":{"clientMutationId":null}}}"#));
        assert_eq!(mark(&*fake, "tok", "PR_1", "a", false), Ok(()));
    }
}

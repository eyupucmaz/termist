//! The GraphQL queries termist sends, and their answers turned into termist's types.
//! Repos go in as aliases (`r0`, `r1`, …): one request reads many repos.
use super::accounts::Permission;
use serde_json::Value;
use termist_core::github::{
    Checks, GhState, Mergeable, PrState, PrSummary, ReviewDecision, ReviewState,
};

/// Open PRs read per repo; a repo with more shows `+N more`.
pub const INBOX_LIMIT: u32 = 50;

/// What a list row and a detail head need.
pub const PR_FIELDS: &str = "fragment PrFields on PullRequest {
  number title url isDraft state createdAt updatedAt headRefName baseRefName
  additions deletions changedFiles mergeable reviewDecision
  author { login }
  reviewRequests(first: 10) { nodes { requestedReviewer { __typename ... on User { login } ... on Team { slug } } } }
  latestOpinionatedReviews(first: 10) { nodes { state author { login } } }
  commits(last: 1) { nodes { commit { statusCheckRollup { state } } } }
}
";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rate {
    pub remaining: u32,
    pub reset_at: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InboxReply {
    pub viewer: String,
    pub rate: Option<Rate>,
    /// In the order the repos were asked for: their PRs and how many are open.
    pub repos: Vec<Result<(Vec<PrSummary>, u32), GhState>>,
}

/// A GraphQL string: JSON's quoting is GraphQL's.
fn quoted(s: &str) -> String {
    serde_json::Value::String(s.to_string()).to_string()
}

/// One aliased `repository(…) { body }` per repo.
fn per_repo(repos: &[(String, String)], body: &str) -> String {
    repos
        .iter()
        .enumerate()
        .map(|(i, (owner, name))| {
            format!(
                "  r{i}: repository(owner: {}, name: {}) {{ {body} }}\n",
                quoted(owner),
                quoted(name)
            )
        })
        .collect()
}

pub fn inbox(repos: &[(String, String)]) -> String {
    let body = format!(
        "pullRequests(states: OPEN, first: {INBOX_LIMIT}, orderBy: {{field: UPDATED_AT, direction: DESC}}) \
         {{ totalCount nodes {{ ...PrFields }} }}"
    );
    format!(
        "{PR_FIELDS}query {{\n  viewer {{ login }}\n  rateLimit {{ remaining resetAt }}\n{}}}\n",
        per_repo(repos, &body)
    )
}

pub fn permissions(repos: &[(String, String)]) -> String {
    format!("query {{\n{}}}\n", per_repo(repos, "viewerPermission"))
}

pub fn counts(repos: &[(String, String)]) -> String {
    format!(
        "query {{\n{}}}\n",
        per_repo(repos, "pullRequests(states: OPEN) { totalCount }")
    )
}

/// The `rN` answers, in order; `Null` for one that is not there.
fn aliases(v: &Value, count: usize) -> impl Iterator<Item = &Value> {
    (0..count).map(move |i| &v["data"][format!("r{i}").as_str()])
}

pub fn parse_rate(v: &Value) -> Option<Rate> {
    Some(Rate {
        remaining: v["remaining"].as_u64()? as u32,
        reset_at: v["resetAt"].as_str()?.to_string(),
    })
}

pub fn parse_inbox(v: &Value, count: usize) -> InboxReply {
    let viewer = v["data"]["viewer"]["login"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let repos = aliases(v, count)
        .map(|r| {
            if r.is_null() {
                return Err(GhState::NoAccess);
            }
            let list = &r["pullRequests"];
            let prs = list["nodes"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|p| summary(p, &viewer))
                .collect();
            Ok((prs, list["totalCount"].as_u64().unwrap_or(0) as u32))
        })
        .collect();
    InboxReply {
        rate: parse_rate(&v["data"]["rateLimit"]),
        viewer,
        repos,
    }
}

pub fn parse_permissions(v: &Value, count: usize) -> Vec<Option<Permission>> {
    aliases(v, count)
        .map(|r| r["viewerPermission"].as_str().and_then(Permission::parse))
        .collect()
}

pub fn parse_counts(v: &Value, count: usize) -> Vec<Option<u32>> {
    aliases(v, count)
        .map(|r| r["pullRequests"]["totalCount"].as_u64().map(|n| n as u32))
        .collect()
}

/// A `PrFields` object; `viewer` is your login, for `requested_you`.
pub fn summary(p: &Value, viewer: &str) -> Option<PrSummary> {
    let text = |v: &Value| v.as_str().unwrap_or_default().to_string();
    let count = |v: &Value| v.as_u64().unwrap_or(0) as u32;
    let requested: Vec<String> = p["reviewRequests"]["nodes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|r| {
            let who = &r["requestedReviewer"];
            who["login"]
                .as_str()
                .map(str::to_string)
                .or_else(|| who["slug"].as_str().map(|s| format!("@{s}")))
        })
        .collect();
    let requested_you = !viewer.is_empty() && requested.iter().any(|r| r == viewer);
    Some(PrSummary {
        number: p["number"].as_u64()? as u32,
        title: text(&p["title"]),
        url: text(&p["url"]),
        author: p["author"]["login"].as_str().unwrap_or("ghost").to_string(),
        draft: p["isDraft"].as_bool().unwrap_or(false),
        state: match p["state"].as_str() {
            Some("MERGED") => PrState::Merged,
            Some("CLOSED") => PrState::Closed,
            _ => PrState::Open,
        },
        created_at: text(&p["createdAt"]),
        updated_at: text(&p["updatedAt"]),
        head: text(&p["headRefName"]),
        base: text(&p["baseRefName"]),
        additions: count(&p["additions"]),
        deletions: count(&p["deletions"]),
        changed_files: count(&p["changedFiles"]),
        mergeable: match p["mergeable"].as_str() {
            Some("MERGEABLE") => Mergeable::Yes,
            Some("CONFLICTING") => Mergeable::Conflicting,
            _ => Mergeable::Unknown,
        },
        decision: match p["reviewDecision"].as_str() {
            Some("APPROVED") => Some(ReviewDecision::Approved),
            Some("CHANGES_REQUESTED") => Some(ReviewDecision::ChangesRequested),
            Some("REVIEW_REQUIRED") => Some(ReviewDecision::ReviewRequired),
            _ => None,
        },
        requested,
        requested_you,
        verdicts: p["latestOpinionatedReviews"]["nodes"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|r| {
                Some((
                    r["author"]["login"].as_str()?.to_string(),
                    review_state(r["state"].as_str()?)?,
                ))
            })
            .collect(),
        checks: match p["commits"]["nodes"][0]["commit"]["statusCheckRollup"]["state"].as_str() {
            Some("SUCCESS") => Checks::Passing,
            Some("FAILURE" | "ERROR") => Checks::Failing,
            Some("PENDING" | "EXPECTED") => Checks::Pending,
            _ => Checks::None,
        },
        unseen: false,
    })
}

pub fn review_state(s: &str) -> Option<ReviewState> {
    Some(match s {
        "APPROVED" => ReviewState::Approved,
        "CHANGES_REQUESTED" => ReviewState::ChangesRequested,
        "COMMENTED" => ReviewState::Commented,
        "DISMISSED" => ReviewState::Dismissed,
        "PENDING" => ReviewState::Pending,
        _ => return None,
    })
}

#[cfg(test)]
pub mod tests {
    use super::*;

    /// Two repos asked; the second one is gone. Synthetic data.
    pub const INBOX: &str = r#"{"data":{"viewer":{"login":"alice"},
      "rateLimit":{"remaining":4990,"resetAt":"2026-10-02T11:00:00Z"},
      "r0":{"pullRequests":{"totalCount":2,"nodes":[
        {"number":212,"title":"Add a dealer filter","url":"https://github.com/acme/site/pull/212",
         "isDraft":false,"state":"OPEN","createdAt":"2026-10-01T10:00:00Z","updatedAt":"2026-10-02T10:00:00Z",
         "headRefName":"feat/dealer","baseRefName":"main","additions":184,"deletions":32,"changedFiles":9,
         "mergeable":"UNKNOWN","reviewDecision":"REVIEW_REQUIRED","author":{"login":"bob"},
         "reviewRequests":{"nodes":[{"requestedReviewer":{"__typename":"User","login":"alice"}},
                                    {"requestedReviewer":{"__typename":"Team","slug":"web"}}]},
         "latestOpinionatedReviews":{"nodes":[{"state":"APPROVED","author":{"login":"carol"}}]},
         "commits":{"nodes":[{"commit":{"statusCheckRollup":{"state":"SUCCESS"}}}]}},
        {"number":201,"title":"WIP: new header","url":"https://github.com/acme/site/pull/201",
         "isDraft":true,"state":"OPEN","createdAt":"2026-09-27T10:00:00Z","updatedAt":"2026-09-28T10:00:00Z",
         "headRefName":"wip","baseRefName":"main","additions":3,"deletions":1,"changedFiles":1,
         "mergeable":"CONFLICTING","reviewDecision":null,"author":null,
         "reviewRequests":{"nodes":[]},"latestOpinionatedReviews":{"nodes":[]},
         "commits":{"nodes":[{"commit":{"statusCheckRollup":null}}]}}]}},
      "r1":null},
      "errors":[{"type":"NOT_FOUND","path":["r1"],"message":"Could not resolve to a Repository with the name 'acme/gone'."}]}"#;

    fn json(s: &str) -> Value {
        serde_json::from_str(s).unwrap()
    }

    #[test]
    fn the_inbox_query_asks_each_repo_under_an_alias() {
        let q = inbox(&[
            ("acme".into(), "site".into()),
            ("ac\"me".into(), "gone".into()),
        ]);
        assert!(q.starts_with("fragment PrFields on PullRequest"));
        assert!(q.contains("viewer { login }"));
        assert!(q.contains("rateLimit { remaining resetAt }"));
        assert!(q.contains(r#"r0: repository(owner: "acme", name: "site")"#));
        assert!(q.contains(r#"r1: repository(owner: "ac\"me", name: "gone")"#));
        assert!(q.contains("pullRequests(states: OPEN, first: 50"));
        assert!(q.contains("...PrFields"));
    }

    #[test]
    fn the_inbox_answer_becomes_rows() {
        let reply = parse_inbox(&json(INBOX), 2);
        assert_eq!(reply.viewer, "alice");
        assert_eq!(
            reply.rate,
            Some(Rate {
                remaining: 4990,
                reset_at: "2026-10-02T11:00:00Z".into()
            })
        );
        assert_eq!(reply.repos[1], Err(GhState::NoAccess));
        let (prs, total) = reply.repos[0].clone().unwrap();
        assert_eq!(total, 2);
        let a = &prs[0];
        assert_eq!(
            (a.number, a.title.as_str(), a.author.as_str()),
            (212, "Add a dealer filter", "bob")
        );
        assert_eq!(a.requested, ["alice", "@web"]);
        assert!(a.requested_you);
        assert_eq!(a.verdicts, [("carol".to_string(), ReviewState::Approved)]);
        assert_eq!(a.checks, Checks::Passing);
        assert_eq!(a.mergeable, Mergeable::Unknown);
        assert_eq!(a.decision, Some(ReviewDecision::ReviewRequired));
        assert_eq!((a.additions, a.deletions, a.changed_files), (184, 32, 9));
        assert!(!a.unseen, "the daemon decides unseen");
        let b = &prs[1];
        assert!(b.draft);
        assert_eq!(b.author, "ghost");
        assert_eq!(b.mergeable, Mergeable::Conflicting);
        assert_eq!(b.checks, Checks::None);
        assert_eq!(b.decision, None);
        assert!(!b.requested_you);
    }

    #[test]
    fn permissions_and_counts_by_alias() {
        let p = permissions(&[("acme".into(), "site".into())]);
        assert!(p.contains(r#"r0: repository(owner: "acme", name: "site") { viewerPermission }"#));
        let v = json(
            r#"{"data":{"r0":{"viewerPermission":"ADMIN"},"r1":null,"r2":{"viewerPermission":"READ"}}}"#,
        );
        assert_eq!(
            parse_permissions(&v, 3),
            [Some(Permission::Admin), None, Some(Permission::Read)]
        );
        let c = counts(&[("acme".into(), "site".into())]);
        assert!(c.contains("pullRequests(states: OPEN) { totalCount }"));
        let v = json(r#"{"data":{"r0":{"pullRequests":{"totalCount":4}},"r1":null}}"#);
        assert_eq!(parse_counts(&v, 2), [Some(4), None]);
    }
}

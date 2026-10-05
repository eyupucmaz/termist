//! The GraphQL queries termist sends, and their answers turned into termist's types.
//! Repos go in as aliases (`r0`, `r1`, …): one request reads many repos.
use super::accounts::Permission;
use serde_json::Value;
use termist_core::github::{
    Check, CheckState, Checks, Comment, FileChange, GhState, Mergeable, More, PrDetail, PrState,
    PrSummary, Review, ReviewDecision, ReviewState, Side, Thread, Viewed,
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

fn text(v: &Value) -> String {
    v.as_str().unwrap_or_default().to_string()
}

fn opt_text(v: &Value) -> Option<String> {
    v.as_str().filter(|s| !s.is_empty()).map(str::to_string)
}

fn nodes(v: &Value) -> impl Iterator<Item = &Value> {
    v["nodes"].as_array().into_iter().flatten()
}

/// How many of a connection did not come.
fn left(v: &Value) -> u32 {
    let total = v["totalCount"].as_u64().unwrap_or(0) as usize;
    total.saturating_sub(nodes(v).count()) as u32
}

fn comment(c: &Value) -> Comment {
    Comment {
        author: c["author"]["login"].as_str().unwrap_or("ghost").to_string(),
        body: text(&c["body"]),
        created_at: text(&c["createdAt"]),
    }
}

fn check(c: &Value) -> Option<Check> {
    Some(match c["__typename"].as_str()? {
        "CheckRun" => Check {
            name: text(&c["name"]),
            workflow: opt_text(&c["checkSuite"]["workflowRun"]["workflow"]["name"]),
            state: match (c["status"].as_str(), c["conclusion"].as_str()) {
                (Some("COMPLETED"), Some("SUCCESS")) => CheckState::Passed,
                (Some("COMPLETED"), Some("SKIPPED")) => CheckState::Skipped,
                (Some("COMPLETED"), Some("CANCELLED")) => CheckState::Cancelled,
                (Some("COMPLETED"), Some("NEUTRAL" | "STALE")) => CheckState::Neutral,
                (Some("COMPLETED"), _) => CheckState::Failed,
                (Some("IN_PROGRESS"), _) => CheckState::Running,
                _ => CheckState::Queued,
            },
            started_at: opt_text(&c["startedAt"]),
            completed_at: opt_text(&c["completedAt"]),
            url: opt_text(&c["detailsUrl"]),
        },
        "StatusContext" => Check {
            name: text(&c["context"]),
            workflow: None,
            state: match c["state"].as_str() {
                Some("SUCCESS") => CheckState::Passed,
                Some("FAILURE" | "ERROR") => CheckState::Failed,
                _ => CheckState::Queued,
            },
            started_at: opt_text(&c["createdAt"]),
            completed_at: None,
            url: opt_text(&c["targetUrl"]),
        },
        _ => return None,
    })
}

/// A `PrFields` object; `viewer` is your login, for `requested_you`.
pub fn summary(p: &Value, viewer: &str) -> Option<PrSummary> {
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

/// One pull request with its conversation, checks and files.
pub fn detail(owner: &str, name: &str, number: u32) -> String {
    format!(
        "{PR_FIELDS}query {{
  viewer {{ login }}
  rateLimit {{ remaining resetAt }}
  repository(owner: {owner}, name: {name}) {{
    pullRequest(number: {number}) {{
      ...PrFields
      id headRefOid
      body
      comments(first: 100) {{ totalCount nodes {{ author {{ login }} body createdAt }} }}
      reviews(first: 50) {{ totalCount nodes {{ author {{ login }} state body submittedAt }} }}
      reviewThreads(first: 100) {{ totalCount nodes {{
        id isResolved isOutdated path line originalLine diffSide
        comments(first: 50) {{ totalCount nodes {{ author {{ login }} body createdAt diffHunk }} }}
      }} }}
      files(first: 100) {{ totalCount nodes {{ path additions deletions changeType viewerViewedState }} }}
      checks: commits(last: 1) {{ nodes {{ commit {{ statusCheckRollup {{ contexts(first: 100) {{ totalCount nodes {{
        __typename
        ... on CheckRun {{ name status conclusion detailsUrl startedAt completedAt
                          checkSuite {{ workflowRun {{ workflow {{ name }} }} }} }}
        ... on StatusContext {{ context state targetUrl createdAt }}
      }} }} }} }} }} }}
    }}
  }}
}}
",
        owner = quoted(owner),
        name = quoted(name),
    )
}

/// `viewerViewedState`; anything else is not viewed.
pub fn viewed(v: &Value) -> Viewed {
    match v.as_str() {
        Some("VIEWED") => Viewed::Viewed,
        Some("DISMISSED") => Viewed::Dismissed,
        _ => Viewed::Unviewed,
    }
}

pub fn parse_detail(v: &Value) -> Result<PrDetail, GhState> {
    let data = &v["data"];
    let viewer = data["viewer"]["login"].as_str().unwrap_or_default();
    let p = &data["repository"]["pullRequest"];
    if p.is_null() {
        return Err(GhState::NoAccess);
    }
    let summary = summary(p, viewer)
        .ok_or_else(|| GhState::Failed("GitHub sent a pull request without a number".into()))?;
    let contexts = &p["checks"]["nodes"][0]["commit"]["statusCheckRollup"]["contexts"];
    Ok(PrDetail {
        summary,
        id: text(&p["id"]),
        head_oid: text(&p["headRefOid"]),
        body: text(&p["body"]),
        comments: nodes(&p["comments"]).map(comment).collect(),
        reviews: nodes(&p["reviews"])
            .filter_map(|r| {
                Some(Review {
                    author: r["author"]["login"].as_str().unwrap_or("ghost").to_string(),
                    state: review_state(r["state"].as_str()?)?,
                    body: text(&r["body"]),
                    submitted_at: text(&r["submittedAt"]),
                })
            })
            .collect(),
        threads: nodes(&p["reviewThreads"])
            .map(|t| Thread {
                id: text(&t["id"]),
                path: text(&t["path"]),
                line: t["line"]
                    .as_u64()
                    .or_else(|| t["originalLine"].as_u64())
                    .map(|n| n as u32),
                side: match t["diffSide"].as_str() {
                    Some("LEFT") => Side::Left,
                    _ => Side::Right,
                },
                resolved: t["isResolved"].as_bool().unwrap_or(false),
                outdated: t["isOutdated"].as_bool().unwrap_or(false),
                hunk: text(&t["comments"]["nodes"][0]["diffHunk"]),
                comments: nodes(&t["comments"]).map(comment).collect(),
                more: left(&t["comments"]),
            })
            .collect(),
        checks: nodes(contexts).filter_map(check).collect(),
        files: nodes(&p["files"])
            .map(|f| FileChange {
                path: text(&f["path"]),
                additions: f["additions"].as_u64().unwrap_or(0) as u32,
                deletions: f["deletions"].as_u64().unwrap_or(0) as u32,
                change: match f["changeType"].as_str() {
                    Some("ADDED") => 'A',
                    Some("DELETED") => 'D',
                    Some("RENAMED") => 'R',
                    Some("COPIED") => 'C',
                    _ => 'M',
                },
                viewed: viewed(&f["viewerViewedState"]),
            })
            .collect(),
        more: More {
            comments: left(&p["comments"]),
            reviews: left(&p["reviews"]),
            threads: left(&p["reviewThreads"]),
            checks: left(contexts),
            files: left(&p["files"]),
        },
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

    pub const DETAIL: &str = r#"{"data":{"viewer":{"login":"alice"},
      "rateLimit":{"remaining":4980,"resetAt":"2026-10-02T11:00:00Z"},
      "repository":{"pullRequest":{
        "number":212,"title":"Add a dealer filter","url":"https://github.com/acme/site/pull/212",
        "isDraft":false,"state":"OPEN","createdAt":"2026-10-01T10:00:00Z","updatedAt":"2026-10-02T10:00:00Z",
        "headRefName":"feat/dealer","baseRefName":"main","additions":184,"deletions":32,"changedFiles":9,
        "id":"PR_kwDOAcme212","headRefOid":"b4d4d37695dfcb6005acbbc67a9c47a096fbea63",
        "mergeable":"MERGEABLE","reviewDecision":"APPROVED","author":{"login":"bob"},
        "reviewRequests":{"nodes":[]},
        "latestOpinionatedReviews":{"nodes":[{"state":"APPROVED","author":{"login":"carol"}}]},
        "commits":{"nodes":[{"commit":{"statusCheckRollup":{"state":"PENDING"}}}]},
        "body":"Adds a dealer dropdown.\n\nCloses #198.",
        "comments":{"totalCount":1,"nodes":[
          {"author":{"login":"bob"},"body":"Screenshots attached","createdAt":"2026-10-01T10:05:00Z"}]},
        "reviews":{"totalCount":2,"nodes":[
          {"author":{"login":"carol"},"state":"APPROVED","body":"Looks good, one nit below.","submittedAt":"2026-10-02T09:00:00Z"},
          {"author":{"login":"carol"},"state":"COMMENTED","body":"","submittedAt":"2026-10-02T08:59:00Z"}]},
        "reviewThreads":{"totalCount":2,"nodes":[
          {"id":"T1","isResolved":false,"isOutdated":false,"path":"src/search/DealerFilter.tsx","line":42,"originalLine":40,"diffSide":"RIGHT",
           "comments":{"totalCount":2,"nodes":[
             {"author":{"login":"carol"},"body":"This refetches on every mount.","createdAt":"2026-10-02T08:58:00Z",
              "diffHunk":"@@ -38,3 +38,5 @@\n   const dealers = useDealers();\n   const [sel, setSel] = useState<string>();\n+  useEffect(() => fetchAll(), []);"},
             {"author":{"login":"bob"},"body":"Good catch, will fix.","createdAt":"2026-10-02T09:30:00Z","diffHunk":"x"}]}},
          {"id":"T2","isResolved":true,"isOutdated":true,"path":"src/api/client.ts","line":null,"originalLine":10,"diffSide":"LEFT",
           "comments":{"totalCount":3,"nodes":[
             {"author":null,"body":"old","createdAt":"2026-10-01T12:00:00Z","diffHunk":"@@ -10 +10 @@\n-a\n+b"}]}}]},
        "files":{"totalCount":101,"nodes":[
          {"path":"src/search/DealerFilter.tsx","additions":120,"deletions":2,"changeType":"ADDED","viewerViewedState":"VIEWED"},
          {"path":"src/old.ts","additions":0,"deletions":30,"changeType":"DELETED","viewerViewedState":"DISMISSED"},
          {"path":"src/api/client.ts","additions":4,"deletions":0,"changeType":"MODIFIED"},
          {"path":"src/b.ts","additions":0,"deletions":0,"changeType":"RENAMED"}]},
        "checks":{"nodes":[{"commit":{"statusCheckRollup":{"contexts":{"totalCount":4,"nodes":[
          {"__typename":"CheckRun","name":"build","status":"COMPLETED","conclusion":"SUCCESS",
           "detailsUrl":"https://github.com/acme/site/actions/runs/1/job/2","startedAt":"2026-10-02T09:00:00Z",
           "completedAt":"2026-10-02T09:01:12Z","checkSuite":{"workflowRun":{"workflow":{"name":"PR Checks"}}}},
          {"__typename":"CheckRun","name":"e2e","status":"IN_PROGRESS","conclusion":null,"detailsUrl":null,
           "startedAt":"2026-10-02T09:00:00Z","completedAt":null,"checkSuite":{"workflowRun":null}},
          {"__typename":"CheckRun","name":"lint","status":"COMPLETED","conclusion":"TIMED_OUT","detailsUrl":null,
           "startedAt":null,"completedAt":null,"checkSuite":null},
          {"__typename":"StatusContext","context":"vercel","state":"PENDING",
           "targetUrl":"https://vercel.com/acme/site/x","createdAt":"2026-10-02T09:00:00Z"}]}}}}]}
      }}}}"#;

    #[test]
    fn the_detail_query_reads_one_pull_request() {
        let q = detail("acme", "site", 212);
        assert!(q.starts_with("fragment PrFields on PullRequest"));
        assert!(q.contains(r#"repository(owner: "acme", name: "site")"#));
        assert!(q.contains("pullRequest(number: 212)"));
        for part in [
            "id headRefOid",
            "reviewThreads(first: 100)",
            "diffSide",
            "viewerViewedState",
            "diffHunk",
            "files(first: 100)",
            "checks: commits(last: 1)",
            "... on CheckRun",
            "... on StatusContext",
        ] {
            assert!(q.contains(part), "{part}");
        }
    }

    #[test]
    fn the_detail_answer_becomes_a_pull_request() {
        let d = parse_detail(&json(DETAIL)).unwrap();
        assert_eq!(d.summary.number, 212);
        assert_eq!(d.id, "PR_kwDOAcme212");
        assert_eq!(d.head_oid, "b4d4d37695dfcb6005acbbc67a9c47a096fbea63");
        assert_eq!(d.summary.checks, Checks::Pending);
        assert_eq!(d.body, "Adds a dealer dropdown.\n\nCloses #198.");
        assert_eq!(d.comments.len(), 1);
        assert_eq!(d.reviews.len(), 2);
        assert_eq!(d.reviews[0].state, ReviewState::Approved);

        let t1 = &d.threads[0];
        assert_eq!(
            (t1.path.as_str(), t1.line, t1.resolved, t1.more),
            ("src/search/DealerFilter.tsx", Some(42), false, 0)
        );
        assert!(t1.hunk.ends_with("+  useEffect(() => fetchAll(), []);"));
        assert_eq!(t1.comments.len(), 2);
        let t2 = &d.threads[1];
        assert_eq!(
            (t2.line, t2.resolved, t2.outdated, t2.more),
            (Some(10), true, true, 2)
        );
        assert_eq!(t2.comments[0].author, "ghost");
        assert_eq!((t1.side, t2.side), (Side::Right, Side::Left));

        let changes: String = d.files.iter().map(|f| f.change).collect();
        assert_eq!(changes, "ADMR");
        let viewed: Vec<Viewed> = d.files.iter().map(|f| f.viewed).collect();
        assert_eq!(
            viewed,
            [
                Viewed::Viewed,
                Viewed::Dismissed,
                Viewed::Unviewed,
                Viewed::Unviewed
            ]
        );
        assert_eq!(d.more.files, 97);

        let checks: Vec<_> = d
            .checks
            .iter()
            .map(|c| (c.name.as_str(), c.state))
            .collect();
        assert_eq!(
            checks,
            [
                ("build", CheckState::Passed),
                ("e2e", CheckState::Running),
                ("lint", CheckState::Failed),
                ("vercel", CheckState::Queued),
            ]
        );
        assert_eq!(d.checks[0].workflow.as_deref(), Some("PR Checks"));
        assert_eq!(d.checks[1].workflow, None);
        assert_eq!(
            d.checks[3].url.as_deref(),
            Some("https://vercel.com/acme/site/x")
        );
        assert_eq!(d.more.checks, 0);
    }

    #[test]
    fn a_pull_request_that_is_not_there_is_no_access() {
        let v = json(r#"{"data":{"viewer":{"login":"a"},"repository":{"pullRequest":null}}}"#);
        assert_eq!(parse_detail(&v), Err(GhState::NoAccess));
        let v = json(r#"{"data":{"viewer":{"login":"a"},"repository":null}}"#);
        assert_eq!(parse_detail(&v), Err(GhState::NoAccess));
    }
}

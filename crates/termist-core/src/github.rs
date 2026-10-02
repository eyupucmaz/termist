//! Pull requests as the daemon reads them from GitHub (through `gh`) and the TUI
//! shows them. Times travel as GitHub writes them: `2026-10-02T15:33:44Z`.
use serde::{Deserialize, Serialize};

/// A GitHub repo found in a project; the daemon's row id for it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct RepoId(pub i64);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PrRef {
    pub repo: RepoId,
    pub number: u32,
}

/// How reading GitHub went, for a repo or for the whole project.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum GhState {
    /// Read fine, or not read yet (`fetched_at` says which).
    Ok,
    /// The `gh` CLI is not installed (or not found).
    NoGh,
    /// `gh` has no logged-in account, or its token stopped working.
    LoggedOut,
    /// No logged-in account can see the repo.
    NoAccess,
    RateLimited {
        reset_at: String,
    },
    /// Anything else, in gh's words.
    Failed(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PrState {
    Open,
    Merged,
    Closed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Mergeable {
    Yes,
    Conflicting,
    /// GitHub has not worked it out yet.
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReviewDecision {
    Approved,
    ChangesRequested,
    ReviewRequired,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReviewState {
    Approved,
    ChangesRequested,
    Commented,
    Dismissed,
    Pending,
}

/// The checks of a PR's last commit, folded into one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Checks {
    None,
    Passing,
    Failing,
    Pending,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CheckState {
    Passed,
    Failed,
    Running,
    Queued,
    Skipped,
    Neutral,
    Cancelled,
}

/// A row of the inbox; the head of the detail.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrSummary {
    pub number: u32,
    pub title: String,
    pub url: String,
    pub author: String,
    pub draft: bool,
    pub state: PrState,
    pub created_at: String,
    pub updated_at: String,
    pub head: String,
    pub base: String,
    pub additions: u32,
    pub deletions: u32,
    pub changed_files: u32,
    pub mergeable: Mergeable,
    pub decision: Option<ReviewDecision>,
    /// Logins asked for a review; teams as `@slug`.
    pub requested: Vec<String>,
    /// You, by login, are among `requested`.
    pub requested_you: bool,
    /// Each reviewer's latest approval or rejection.
    pub verdicts: Vec<(String, ReviewState)>,
    pub checks: Checks,
    /// Changed since you last opened it, or never opened.
    pub unseen: bool,
}

/// One repo's open pull requests.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoPrs {
    pub repo: RepoId,
    /// The folder's name.
    pub name: String,
    /// `owner/name` on GitHub.
    pub slug: String,
    pub state: GhState,
    /// Your login on the account this repo is read with.
    pub viewer: Option<String>,
    pub prs: Vec<PrSummary>,
    /// Open PRs on GitHub: more than `prs` when over the limit.
    pub total: u32,
    pub fetched_at: Option<String>,
    pub failed_at: Option<String>,
}

/// A repo in the repos window.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoInfo {
    pub id: RepoId,
    pub name: String,
    pub slug: String,
    pub visible: bool,
    /// The account it is read with, chosen or set by you (`pinned`).
    pub account: Option<String>,
    pub pinned: bool,
    pub open_count: Option<u32>,
    pub state: GhState,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Comment {
    pub author: String,
    pub body: String,
    pub created_at: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Review {
    pub author: String,
    pub state: ReviewState,
    pub body: String,
    pub submitted_at: String,
}

/// Comments on a line of the diff.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Thread {
    pub id: String,
    pub path: String,
    /// The line now, else the line it was written on.
    pub line: Option<u32>,
    pub resolved: bool,
    pub outdated: bool,
    /// The diff around the line, from the first comment.
    pub hunk: String,
    pub comments: Vec<Comment>,
    /// Comments beyond those that came.
    pub more: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Check {
    pub name: String,
    pub workflow: Option<String>,
    pub state: CheckState,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    pub url: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileChange {
    pub path: String,
    pub additions: u32,
    pub deletions: u32,
    /// `A`, `M`, `D`, `R` or `C`.
    pub change: char,
}

/// How many of each did not come (over the query's limits).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct More {
    pub comments: u32,
    pub reviews: u32,
    pub threads: u32,
    pub checks: u32,
    pub files: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrDetail {
    pub summary: PrSummary,
    pub body: String,
    pub comments: Vec<Comment>,
    pub reviews: Vec<Review>,
    pub threads: Vec<Thread>,
    pub checks: Vec<Check>,
    pub files: Vec<FileChange>,
    pub more: More,
}

/// Seconds since 1970 of `2026-10-02T15:33:44Z`; `None` for any other shape.
pub fn unix_secs(t: &str) -> Option<i64> {
    let b = t.as_bytes();
    let shape = b.len() == 20
        && b[4] == b'-'
        && b[7] == b'-'
        && b[10] == b'T'
        && b[13] == b':'
        && b[16] == b':'
        && b[19] == b'Z';
    if !shape {
        return None;
    }
    let num = |from: usize, to: usize| -> Option<i64> {
        let s = t.get(from..to)?;
        s.bytes()
            .all(|c| c.is_ascii_digit())
            .then(|| s.parse().ok())?
    };
    let (y, m, d) = (num(0, 4)?, num(5, 7)?, num(8, 10)?);
    let (h, mi, s) = (num(11, 13)?, num(14, 16)?, num(17, 19)?);
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) || h > 23 || mi > 59 || s > 60 {
        return None;
    }
    // Days from the civil date (H. Hinnant's algorithm), March-based years.
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * ((m + 9) % 12) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(days * 86_400 + h * 3600 + mi * 60 + s)
}

/// The other way: `rfc3339(0)` is `1970-01-01T00:00:00Z`.
pub fn rfc3339(unix: i64) -> String {
    let (days, secs) = (unix.div_euclid(86_400), unix.rem_euclid(86_400));
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        secs / 3600,
        secs % 3600 / 60,
        secs % 60
    )
}

/// How long ago, in one unit: `40s`, `3m`, `5h`, `2d`, `3w`, `1y`.
pub fn age(secs: i64) -> String {
    let s = secs.max(0);
    match s {
        0..60 => format!("{s}s"),
        60..3600 => format!("{}m", s / 60),
        3600..86_400 => format!("{}h", s / 3600),
        86_400..604_800 => format!("{}d", s / 86_400),
        604_800..31_536_000 => format!("{}w", s / 604_800),
        _ => format!("{}y", s / 31_536_000),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn github_times_read_as_seconds() {
        assert_eq!(unix_secs("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(unix_secs("2026-10-02T15:33:44Z"), Some(1_790_955_224));
        assert_eq!(unix_secs("2000-02-29T00:00:00Z"), Some(951_782_400));
        assert_eq!(unix_secs("1969-12-31T23:59:59Z"), Some(-1));
    }

    #[test]
    fn other_shapes_are_not_times() {
        for bad in [
            "",
            "2026-10-02",
            "2026-10-02T15:33:44",
            "2026-10-02T15:33:44+03:00",
            "2026-13-02T15:33:44Z",
            "2026-10-02T25:33:44Z",
            "2026-1x-02T15:33:44Z",
        ] {
            assert_eq!(unix_secs(bad), None, "{bad}");
        }
    }

    #[test]
    fn seconds_write_back_as_github_times() {
        assert_eq!(rfc3339(0), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339(1_790_000_000), "2026-09-21T14:13:20Z");
        for t in [-1, 951_782_400, 1_790_955_224] {
            assert_eq!(unix_secs(&rfc3339(t)), Some(t));
        }
    }

    #[test]
    fn ages_use_the_largest_unit() {
        assert_eq!(age(-5), "0s");
        assert_eq!(age(40), "40s");
        assert_eq!(age(3 * 60 + 59), "3m");
        assert_eq!(age(5 * 3600), "5h");
        assert_eq!(age(2 * 86400 + 10), "2d");
        assert_eq!(age(15 * 86400), "2w");
        assert_eq!(age(400 * 86400), "1y");
    }
}

//! When to read GitHub again: an interval by focus, doubling after failures.
use std::time::{Duration, Instant};

/// A project someone looks at.
pub const FOCUSED: Duration = Duration::from_secs(30);
/// Other open projects: for the tab badge and the review toasts.
pub const OPEN: Duration = Duration::from_secs(180);
/// The pull request someone has open.
pub const DETAIL: Duration = Duration::from_secs(20);
/// An account low on its hourly budget.
pub const SLOW: Duration = Duration::from_secs(600);
pub const MAX_BACKOFF: Duration = Duration::from_secs(600);
/// Accounts that could not be loaded (no gh, logged out) are tried again this often.
pub const AUTH_RETRY: Duration = Duration::from_secs(600);
/// Permission questions that failed are asked again this often.
pub const PERMISSIONS: Duration = Duration::from_secs(30);

pub fn every(focused: bool, slow: bool) -> Duration {
    if slow {
        SLOW
    } else if focused {
        FOCUSED
    } else {
        OPEN
    }
}

/// One thing read again and again: never two at once, due at `next`.
#[derive(Clone, Debug, Default)]
pub struct Beat {
    next: Option<Instant>,
    failures: u32,
    in_flight: bool,
    last_ok: Option<Instant>,
}

impl Beat {
    /// Nothing on its way and the time has come (a new beat is due at once).
    pub fn due(&self, now: Instant) -> bool {
        !self.in_flight && self.next.is_none_or(|t| now >= t)
    }

    pub fn start(&mut self) {
        self.in_flight = true;
    }

    /// Next in `every`; after failures `every` doubles each time, up to `MAX_BACKOFF`.
    pub fn finish(&mut self, now: Instant, ok: bool, every: Duration) {
        self.in_flight = false;
        let wait = if ok {
            self.failures = 0;
            self.last_ok = Some(now);
            every
        } else {
            self.failures = self.failures.saturating_add(1);
            every
                .saturating_mul(2u32.saturating_pow(self.failures.min(16)))
                .min(MAX_BACKOFF)
                .max(every)
        };
        self.next = Some(now + wait);
    }

    /// Due now.
    pub fn hurry(&mut self, now: Instant) {
        self.next = Some(now);
    }

    /// Due now if the last good read is older than `age`.
    pub fn freshen(&mut self, now: Instant, age: Duration) {
        if self
            .last_ok
            .is_none_or(|t| now.saturating_duration_since(t) >= age)
        {
            self.next = Some(now);
        }
    }

    /// Never later than `now + every` (the interval just got shorter).
    pub fn cap(&mut self, now: Instant, every: Duration) {
        if let Some(next) = self.next {
            self.next = Some(next.min(now + every));
        }
    }

    pub fn in_flight(&self) -> bool {
        self.in_flight
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: Duration = Duration::from_secs(1);

    #[test]
    fn a_new_beat_is_due_and_not_twice() {
        let now = Instant::now();
        let mut b = Beat::default();
        assert!(b.due(now));
        b.start();
        assert!(!b.due(now + 1000 * S), "one on its way");
        b.finish(now, true, FOCUSED);
        assert!(!b.due(now + 29 * S));
        assert!(b.due(now + 30 * S));
    }

    #[test]
    fn failures_double_up_to_ten_minutes_and_a_success_resets() {
        let now = Instant::now();
        let mut b = Beat::default();
        let mut waits = vec![];
        for _ in 0..6 {
            b.start();
            b.finish(now, false, FOCUSED);
            let wait = (1..=700).find(|s| b.due(now + *s * S)).unwrap();
            waits.push(wait);
        }
        assert_eq!(waits, [60, 120, 240, 480, 600, 600]);
        b.start();
        b.finish(now, true, FOCUSED);
        assert!(b.due(now + 30 * S));
    }

    #[test]
    fn hurry_freshen_and_cap() {
        let now = Instant::now();
        let mut b = Beat::default();
        b.start();
        b.finish(now, true, OPEN);
        b.freshen(now + 10 * S, FOCUSED);
        assert!(!b.due(now + 10 * S), "read 10 s ago is fresh");
        b.freshen(now + 40 * S, FOCUSED);
        assert!(b.due(now + 40 * S), "read 40 s ago is not");
        b.start();
        b.finish(now, true, OPEN);
        b.cap(now, FOCUSED);
        assert!(b.due(now + 30 * S));
        b.start();
        b.hurry(now);
        assert!(!b.due(now), "hurry never doubles a read on its way");
    }

    #[test]
    fn the_interval_follows_focus_and_budget() {
        assert_eq!(every(true, false), FOCUSED);
        assert_eq!(every(false, false), OPEN);
        assert_eq!(every(true, true), SLOW);
    }
}

//! Short notices shown in the status bar for a while.

use std::time::{Duration, Instant};

/// What a notice is about, which sets its colour.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Something worked, such as a commit.
    Info,
    /// Something did not happen: a failure in git's or a hook's words, or
    /// a refusal in termis's.
    Error,
}

/// A notice and when it goes away.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Toast {
    pub text: String,
    pub kind: Kind,
    until: Instant,
}

impl Toast {
    /// A notice shown from `now` for `duration`.
    pub fn new(text: String, kind: Kind, now: Instant, duration: Duration) -> Self {
        Self {
            text,
            kind,
            until: now + duration,
        }
    }

    /// Whether the notice is still to be shown at `now`.
    pub fn live(&self, now: Instant) -> bool {
        now < self.until
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_toast_lives_for_its_duration() {
        let t0 = Instant::now();
        let toast = Toast::new("done".into(), Kind::Info, t0, Duration::from_secs(5));
        assert!(toast.live(t0));
        assert!(toast.live(t0 + Duration::from_millis(4999)));
        assert!(!toast.live(t0 + Duration::from_secs(5)));
    }
}

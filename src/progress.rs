//! Work-owned progress marks (#1153).
//!
//! `swarm serve` feeds the systemd watchdog from a side thread while it
//! answers an ask. Fed on a timer alone, that thread cannot tell a slow answer
//! from a stuck one: it vouches for the process without looking at the work.
//! This module is the second clock. The thread doing the work installs a
//! [`Progress`] with [`install`], and the slow paths it calls (recall stages,
//! the scan loops, the model request) advance it with [`mark`]. The watchdog
//! thread holds the same `Arc` but can only read it: advancing goes through
//! the calling thread's installed scope, and the watchdog thread has none.
//!
//! With nothing installed (the CLI, tests, every other daemon) [`mark`] is one
//! thread-local read and does nothing else.

use std::cell::RefCell;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// How far one piece of work has got, and when it last moved.
#[derive(Debug)]
pub struct Progress {
    anchor: Instant,
    /// The latest mark, packed so a reader sees one consistent pair: the high
    /// 32 bits are the silence it declared it may need (ms; a blocking call
    /// with its own timeout, 0 = none beyond the reader's stall window), the
    /// low 32 bits are when it landed (ms after `anchor`). Kept apart, a
    /// reader could see a new plain mark's zero allowance with the previous
    /// bounded mark's old time and call a healthy ask stalled.
    latest: AtomicU64,
    marks: AtomicU64,
    phase: Mutex<&'static str>,
}

/// Milliseconds as `u32`, saturating (49 days is far past any ask).
fn ms32(d: Duration) -> u64 {
    u64::from(u32::try_from(d.as_millis()).unwrap_or(u32::MAX))
}

impl Progress {
    /// A fresh mark, phase `start`: the work counts as moving from creation.
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            anchor: Instant::now(),
            latest: AtomicU64::new(0),
            marks: AtomicU64::new(0),
            phase: Mutex::new("start"),
        })
    }

    /// One consistent reading of the latest mark: the time since it, and the
    /// silence it declared it may need (zero for a plain mark).
    pub fn snapshot(&self) -> (Duration, Duration) {
        let latest = self.latest.load(Ordering::Acquire);
        let last = Duration::from_millis(latest & u64::from(u32::MAX));
        let allow = Duration::from_millis(latest >> 32);
        (self.anchor.elapsed().saturating_sub(last), allow)
    }

    /// Time since the latest mark.
    pub fn since_last(&self) -> Duration {
        self.snapshot().0
    }

    /// Silence the latest mark said it may need (zero for a plain mark).
    pub fn allowance(&self) -> Duration {
        self.snapshot().1
    }

    /// Number of marks so far.
    pub fn marks(&self) -> u64 {
        self.marks.load(Ordering::Acquire)
    }

    /// Phase named by the latest mark.
    pub fn phase(&self) -> &'static str {
        *self.phase.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn advance(&self, phase: &'static str, allow: Duration) {
        *self.phase.lock().unwrap_or_else(|e| e.into_inner()) = phase;
        let latest = (ms32(allow) << 32) | ms32(self.anchor.elapsed());
        self.latest.store(latest, Ordering::Release);
        self.marks.fetch_add(1, Ordering::AcqRel);
    }
}

thread_local! {
    static CURRENT: RefCell<Option<Arc<Progress>>> = const { RefCell::new(None) };
}

/// Restores the previously installed progress (if any) on drop.
pub struct ProgressScope {
    prev: Option<Arc<Progress>>,
}

impl Drop for ProgressScope {
    fn drop(&mut self) {
        let prev = self.prev.take();
        CURRENT.with(|c| *c.borrow_mut() = prev);
    }
}

/// Make `progress` the one this thread's [`mark`] calls advance, until the
/// returned scope drops.
pub fn install(progress: Arc<Progress>) -> ProgressScope {
    let prev = CURRENT.with(|c| c.borrow_mut().replace(progress));
    ProgressScope { prev }
}

/// The work moved: record it on this thread's installed progress, if any.
pub fn mark(phase: &'static str) {
    mark_bounded(phase, Duration::ZERO);
}

/// As [`mark`], for a phase about to block in one call that has its own
/// timeout `bound` (a non-streaming model request). The reader may then wait
/// up to `bound` for the next mark instead of its stall window, so a long but
/// bounded call is not mistaken for a hang, and a hang inside it is still
/// ended by the call's own timeout.
pub fn mark_bounded(phase: &'static str, bound: Duration) {
    CURRENT.with(|c| {
        if let Some(p) = c.borrow().as_ref() {
            p.advance(phase, bound);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mark_advances_only_the_installed_progress() {
        let p = Progress::new();
        mark("nothing installed");
        assert_eq!(p.marks(), 0);
        {
            let _scope = install(p.clone());
            mark("recall");
            assert_eq!(p.marks(), 1);
            assert_eq!(p.phase(), "recall");
        }
        mark("after scope");
        assert_eq!(p.marks(), 1, "a dropped scope must stop advancing");
    }

    #[test]
    fn another_thread_cannot_advance_it() {
        let p = Progress::new();
        let _scope = install(p.clone());
        let q = p.clone();
        std::thread::spawn(move || {
            mark("other thread");
            assert_eq!(q.marks(), 0);
        })
        .join()
        .unwrap();
        assert_eq!(p.marks(), 0);
    }

    #[test]
    fn bounded_mark_carries_its_allowance_and_a_plain_mark_clears_it() {
        let p = Progress::new();
        let _scope = install(p.clone());
        mark_bounded("model", Duration::from_secs(330));
        assert_eq!(p.allowance(), Duration::from_secs(330));
        mark("model replied");
        assert_eq!(p.allowance(), Duration::ZERO);
        assert!(p.since_last() < Duration::from_secs(5));
    }

    #[test]
    fn a_reader_never_sees_a_torn_mark() {
        // #1153 round two. The writer alternates a bounded mark (big
        // allowance) and, after a silence longer than the window, a plain one.
        // Kept as two fields, a reader could catch the plain mark's zero
        // allowance with the bounded mark's old time and see a false stall.
        // Counted only while the completed-mark count is odd and unchanged
        // across the read (the bounded mark is the latest finished one), so a
        // writer merely descheduled after its plain mark is not miscounted.
        let p = Progress::new();
        let window = Duration::from_millis(2);
        let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let writer = {
            let (p, done) = (p.clone(), done.clone());
            std::thread::spawn(move || {
                let _scope = install(p);
                for _ in 0..400 {
                    mark_bounded("model request", Duration::from_secs(3600));
                    std::thread::sleep(Duration::from_millis(4));
                    mark("model reply");
                }
                done.store(true, Ordering::Release);
            })
        };
        let mut torn = 0u64;
        while !done.load(Ordering::Acquire) {
            let before = p.marks();
            let (since, allow) = p.snapshot();
            if before % 2 == 1 && p.marks() == before && allow.is_zero() && since > window {
                torn += 1;
            }
        }
        writer.join().unwrap();
        assert_eq!(torn, 0, "{torn} readings of a moving mark looked stalled");
    }

    #[test]
    fn huge_durations_saturate_instead_of_wrapping() {
        let p = Progress::new();
        let _scope = install(p.clone());
        mark_bounded("model request", Duration::from_secs(u64::MAX / 4));
        assert_eq!(p.allowance(), Duration::from_millis(u64::from(u32::MAX)));
        assert!(p.since_last() < Duration::from_secs(5), "the time half must be intact");
    }
}

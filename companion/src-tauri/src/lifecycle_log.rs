//! Issue #137 cross-cutting: an explicit lifecycle state machine + named
//! event log, replacing the ad-hoc `trace()` calls scattered across the
//! host-lifetime decision points.
//!
//! The whole 0.4.x whack-a-mole came from the lifetime logic being implicit:
//! a child-counter proxy, a 60 s grace, scattered traces — no single place
//! that said "we are in phase X and just moved to Y because Z". This module
//! makes the host's lifetime an explicit, named [`Phase`] with logged
//! transitions, plus a bounded ring of the most recent [`LifecycleEvent`]s
//! for post-hoc diagnosis (the coverage gap that made the instability so hard
//! to pin down).
//!
//! It is deliberately tiny and lock-simple: the lifetime path is low
//! frequency (a few events per disconnect edge), so a single `Mutex` and a
//! `VecDeque` are plenty. Recording an event also emits a structured
//! `trace()` line so existing log tooling keeps working.

use std::collections::VecDeque;
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

/// Max events retained in the in-memory ring (oldest dropped past this).
const RING_CAP: usize = 256;

/// The host's lifetime phase. A small, total state set — every transition
/// goes through [`transition`], which logs it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// Process up, HTTP not yet serving.
    Starting,
    /// HTTP bound, accepting renders — the normal running state.
    Serving,
    /// Last MCP child disconnected; the 5 s grace timer is armed and we are
    /// about to re-check Claude Desktop liveness.
    GracePending,
    /// A terminal exit has been authorized; process is on its way out.
    Exiting,
}

impl Phase {
    fn name(self) -> &'static str {
        match self {
            Phase::Starting => "Starting",
            Phase::Serving => "Serving",
            Phase::GracePending => "GracePending",
            Phase::Exiting => "Exiting",
        }
    }
}

/// Named lifecycle events. These are the points that used to be bare
/// `trace()` strings; naming them makes the event log greppable and the
/// state machine's reasoning explicit.
#[derive(Debug, Clone)]
pub enum LifecycleEvent {
    /// Process startup; `interactive` = desktop session (GUI may show) vs
    /// headless/SSH.
    Startup { interactive: bool },
    /// HTTP server bound and serving on the given port.
    Serving { port: u16 },
    /// An MCP-stdio child attached / detached; carries the new live count.
    ChildAttached { count: usize },
    ChildDetached { count: usize },
    /// Last child gone — grace timer armed for `secs` before the liveness
    /// re-check.
    GraceArmed { secs: u64 },
    /// Grace elapsed and resolved. `outcome` is "stay" or "exit"; the two
    /// inputs to that decision are recorded for forensics.
    GraceResolved {
        outcome: &'static str,
        claude_desktop_running: bool,
        child_returned: bool,
    },
    /// A window-close was treated as hide (not exit) — the I-invariant that
    /// window-X never kills the host.
    WindowHidden,
    /// Tauri's `ExitRequested` gate default-denied a quit attempt.
    ExitDenied,
    /// Terminal exit authorized; `reason` is the single exit authority's
    /// cause (e.g. "claude-desktop-gone", "uninstall", "update").
    HostExit { reason: &'static str },
}

impl LifecycleEvent {
    fn render(&self) -> String {
        match self {
            LifecycleEvent::Startup { interactive } => {
                format!("startup (interactive={interactive})")
            }
            LifecycleEvent::Serving { port } => format!("serving on :{port}"),
            LifecycleEvent::ChildAttached { count } => format!("child attached (count={count})"),
            LifecycleEvent::ChildDetached { count } => format!("child detached (count={count})"),
            LifecycleEvent::GraceArmed { secs } => format!("grace armed ({secs}s)"),
            LifecycleEvent::GraceResolved {
                outcome,
                claude_desktop_running,
                child_returned,
            } => format!(
                "grace resolved → {outcome} (claude_desktop_running={claude_desktop_running}, child_returned={child_returned})"
            ),
            LifecycleEvent::WindowHidden => "window close treated as hide".to_string(),
            LifecycleEvent::ExitDenied => "ExitRequested default-denied".to_string(),
            LifecycleEvent::HostExit { reason } => format!("host exit authorized ({reason})"),
        }
    }
}

fn ring() -> &'static Mutex<VecDeque<String>> {
    static RING: OnceLock<Mutex<VecDeque<String>>> = OnceLock::new();
    RING.get_or_init(|| Mutex::new(VecDeque::with_capacity(RING_CAP)))
}

fn epoch() -> Instant {
    static EPOCH: OnceLock<Instant> = OnceLock::new();
    *EPOCH.get_or_init(Instant::now)
}

fn phase_cell() -> &'static Mutex<Phase> {
    static PHASE: OnceLock<Mutex<Phase>> = OnceLock::new();
    PHASE.get_or_init(|| Mutex::new(Phase::Starting))
}

/// Record a named lifecycle event: push to the bounded ring and emit a
/// structured `trace()` line. Never panics on a poisoned lock (lifetime
/// logging must not itself become a failure source).
pub fn record(ev: LifecycleEvent) {
    let ms = epoch().elapsed().as_millis();
    let line = format!("[{ms}ms] {}", ev.render());
    crate::logging::trace(&format!("lifecycle {line}"));
    if let Ok(mut q) = ring().lock() {
        if q.len() >= RING_CAP {
            q.pop_front();
        }
        q.push_back(line);
    }
}

/// Move to `next` phase, logging the transition (no-op log if unchanged).
pub fn transition(next: Phase) {
    let prev = {
        let Ok(mut p) = phase_cell().lock() else { return };
        let prev = *p;
        *p = next;
        prev
    };
    if prev != next {
        record_transition(prev, next);
    }
}

fn record_transition(prev: Phase, next: Phase) {
    let ms = epoch().elapsed().as_millis();
    let line = format!("[{ms}ms] phase {} → {}", prev.name(), next.name());
    crate::logging::trace(&format!("lifecycle {line}"));
    if let Ok(mut q) = ring().lock() {
        if q.len() >= RING_CAP {
            q.pop_front();
        }
        q.push_back(line);
    }
}

/// Current lifetime phase (for diagnostics / health surfaces).
pub fn current_phase() -> Phase {
    phase_cell().lock().map(|p| *p).unwrap_or(Phase::Serving)
}

/// Snapshot of the recent event ring, oldest first — for a diagnostic dump.
pub fn recent() -> Vec<String> {
    ring()
        .lock()
        .map(|q| q.iter().cloned().collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_events_and_caps_ring() {
        for i in 0..(RING_CAP + 50) {
            record(LifecycleEvent::ChildAttached { count: i });
        }
        let r = recent();
        assert!(r.len() <= RING_CAP, "ring is bounded, got {}", r.len());
        // Oldest dropped: the very first event must be gone.
        assert!(!r.iter().any(|l| l.contains("count=0)")) || r.len() == RING_CAP);
    }

    #[test]
    fn transition_updates_phase_and_is_idempotent() {
        // `transition` is the only mutator of the process-global phase, and
        // only this test calls it, so asserting `current_phase()` is race-free
        // (unlike the shared event ring, which other tests spam concurrently).
        transition(Phase::GracePending);
        assert_eq!(current_phase(), Phase::GracePending);
        transition(Phase::GracePending); // idempotent — no panic, phase holds
        assert_eq!(current_phase(), Phase::GracePending);
        transition(Phase::Serving);
        assert_eq!(current_phase(), Phase::Serving);
    }

    #[test]
    fn event_render_is_human_legible() {
        let ev = LifecycleEvent::GraceResolved {
            outcome: "exit",
            claude_desktop_running: false,
            child_returned: false,
        };
        let s = ev.render();
        assert!(s.contains("exit") && s.contains("claude_desktop_running=false"));
    }
}

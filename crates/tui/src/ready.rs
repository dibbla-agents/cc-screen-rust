//! Proposal 0018 §2 — the shared client-side "ready" edge detector for `ccs`,
//! the Rust sibling of `frontend/src/readyEdges.ts`. A session becomes
//! *notifiable* on the same gated busy→waiting edge the server gates the OS Web
//! Push on (0002, `engine.rs::notification_eligible`), recomputed here from two
//! consecutive 1 s poll snapshots.
//!
//! Kept pure and side-effect-free so it unit-tests from two `SessionInfo`
//! snapshots (see the tests below). The app-side glue (prev-snapshot ref, focus
//! gate, toast plumbing) lives in `app.rs`.

use std::collections::{HashMap, HashSet};

use cc_screen_protocol::SessionInfo;

// Defaults match the server gate (engine.rs NOTIFY_MIN_WORK_SECS /
// NOTIFY_INPUT_QUIET_SECS) and the web's readyEdges.ts, so the TUI toast, the
// web toast, and the OS push all fire on exactly the same edge.
pub const NOTIFY_MIN_WORK_SECS: u64 = 60;
pub const NOTIFY_INPUT_QUIET_SECS: u64 = 60;

/// The identity used everywhere a session is keyed across machines: a same-named
/// session on a different agent is a different session. Mirrors `app.rs`'s
/// pane-liveness key so the mounted-exclusion set lines up.
pub fn session_key(s: &SessionInfo) -> (String, String) {
    (s.machine.clone(), s.name.clone())
}

/// One ready edge worth a toast — just enough for the toast row (short name +
/// tool) and to route the jump through `fill_box`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadyEdge {
    pub name: String,
    pub machine: String,
    pub tool: String,
    pub short: String,
}

impl ReadyEdge {
    fn from(s: &SessionInfo) -> Self {
        Self {
            name: s.name.clone(),
            machine: s.machine.clone(),
            tool: s.tool.clone(),
            short: s.short.clone(),
        }
    }
    /// The cross-machine key, matching `session_key`.
    pub fn key(&self) -> (String, String) {
        (self.machine.clone(), self.name.clone())
    }
}

/// Diff the previous snapshot against the current one and return the sessions
/// that crossed the gated busy→waiting edge per §2. `mounted` is the set of
/// `session_key`s currently on screen in a pane (never notified); `now` is the
/// current Unix time in seconds — passed in so the function stays pure.
///
/// Suppression rules (mirroring the server's "first sight records state only"):
///   - a session absent from `prev` is first-seen → baseline only, no edge;
///   - a session in `mounted` is on screen → carries its own status, no edge;
///   - the focus gate and the first-snapshot-after-load baseline are enforced by
///     the caller (`app.rs`), which simply doesn't act on a hidden terminal /
///     with no prior snapshot.
pub fn detect_ready_edges(
    prev: &[SessionInfo],
    cur: &[SessionInfo],
    mounted: &HashSet<(String, String)>,
    now: u64,
) -> Vec<ReadyEdge> {
    let prev_by: HashMap<(String, String), &SessionInfo> =
        prev.iter().map(|s| (session_key(s), s)).collect();
    // Treat negative ages as 0 — same discipline as the server's saturating_sub,
    // so clock skew can't spuriously satisfy a gate.
    let age = |t: u64| now.saturating_sub(t);

    let mut edges = Vec::new();
    for c in cur {
        let key = session_key(c);
        if mounted.contains(&key) {
            continue; // on screen — carries its own status
        }
        let Some(p) = prev_by.get(&key) else {
            continue; // first sight — establish baseline, don't notify
        };

        // busy → waiting edge.
        if p.waiting || !c.waiting {
            continue;
        }
        if c.busy_since == 0 {
            continue; // never recorded a work start (server gate)
        }
        if age(c.busy_since) < NOTIFY_MIN_WORK_SECS {
            continue; // gate 1: worked > 1 min
        }
        if age(c.last_input_at) < NOTIFY_INPUT_QUIET_SECS {
            continue; // gate 2: user idle > 1 min
        }
        edges.push(ReadyEdge::from(c));
    }
    edges
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sess(name: &str, waiting: bool, busy_since: u64, last_input_at: u64) -> SessionInfo {
        SessionInfo {
            name: name.into(),
            tool: "claude".into(),
            short: name.into(),
            attached: false,
            activity: 0,
            last_input_at,
            busy_since,
            busy_until: 0,
            preview: String::new(),
            waiting,
            skip_permissions: None,
            cwd: String::new(),
            machine: String::new(),
            headline: None,
            detail: None,
            color: None,
        }
    }

    const NOW: u64 = 1_000_000;
    fn long_ago() -> u64 {
        NOW - (NOTIFY_MIN_WORK_SECS + 5)
    }
    fn no_mounted() -> HashSet<(String, String)> {
        HashSet::new()
    }

    #[test]
    fn fires_on_a_gated_busy_to_waiting_edge() {
        let prev = vec![sess("a", false, long_ago(), long_ago())];
        let cur = vec![sess("a", true, long_ago(), long_ago())];
        let edges = detect_ready_edges(&prev, &cur, &no_mounted(), NOW);
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].name, "a");
    }

    #[test]
    fn rejects_short_work_turn() {
        let prev = vec![sess("a", false, NOW - 10, long_ago())];
        let cur = vec![sess("a", true, NOW - 10, long_ago())]; // only worked 10s
        assert!(detect_ready_edges(&prev, &cur, &no_mounted(), NOW).is_empty());
    }

    #[test]
    fn rejects_recent_user_input() {
        let prev = vec![sess("a", false, long_ago(), NOW - 5)];
        let cur = vec![sess("a", true, long_ago(), NOW - 5)]; // typed 5s ago
        assert!(detect_ready_edges(&prev, &cur, &no_mounted(), NOW).is_empty());
    }

    #[test]
    fn rejects_busy_since_zero() {
        // No recorded work start (e.g. an older agent) → never notifies.
        let prev = vec![sess("a", false, 0, long_ago())];
        let cur = vec![sess("a", true, 0, long_ago())];
        assert!(detect_ready_edges(&prev, &cur, &no_mounted(), NOW).is_empty());
    }

    #[test]
    fn first_snapshot_is_baseline_only() {
        // Already waiting on first sight (absent from prev) → no edge.
        let cur = vec![sess("a", true, long_ago(), long_ago())];
        assert!(detect_ready_edges(&[], &cur, &no_mounted(), NOW).is_empty());
    }

    #[test]
    fn no_edge_when_still_busy() {
        let prev = vec![sess("a", false, long_ago(), long_ago())];
        let cur = vec![sess("a", false, long_ago(), long_ago())];
        assert!(detect_ready_edges(&prev, &cur, &no_mounted(), NOW).is_empty());
    }

    #[test]
    fn excludes_mounted_sessions() {
        let prev = vec![sess("a", false, long_ago(), long_ago())];
        let cur = vec![sess("a", true, long_ago(), long_ago())];
        let mut mounted = HashSet::new();
        mounted.insert((String::new(), "a".to_string()));
        assert!(detect_ready_edges(&prev, &cur, &mounted, NOW).is_empty());
    }

    #[test]
    fn future_timestamps_clamp_to_zero_age() {
        // busy_since / last_input_at in the future ⇒ negative age ⇒ clamped to 0
        // ⇒ both gates reject (acts like "just started"). No panic on underflow.
        let prev = vec![sess("a", false, NOW + 100, NOW + 100)];
        let cur = vec![sess("a", true, NOW + 100, NOW + 100)];
        assert!(detect_ready_edges(&prev, &cur, &no_mounted(), NOW).is_empty());
    }

    #[test]
    fn machine_keys_dont_cross_notify() {
        // Same name, different machines: only the one that crossed the edge fires.
        let mut p_studio = sess("a", false, long_ago(), long_ago());
        p_studio.machine = "studio".into();
        let mut c_studio = sess("a", true, long_ago(), long_ago());
        c_studio.machine = "studio".into();
        let mut pine = sess("a", false, long_ago(), long_ago());
        pine.machine = "pine".into();

        let edges =
            detect_ready_edges(&[p_studio, pine.clone()], &[c_studio, pine], &no_mounted(), NOW);
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].machine, "studio");
    }
}

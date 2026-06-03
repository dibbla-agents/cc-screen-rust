//! Web Push wiring for the agent. The reusable machinery (VAPID keypair,
//! subscription store, `notify`) lives in `cc-screen-push` and is re-exported
//! here; this module keeps `finish_watcher`, the AppState-coupled busy→waiting
//! sweep that buzzes a *directly*-connected client's devices.
//!
//! Under a hub the hub owns push (one keypair + sub store for the whole fleet);
//! the agent's uplink emits `WaitingEdge` on the same busy→waiting edge and the
//! hub fans it out. Both can run at once (dual-mode): a device subscribed to the
//! agent directly is served here; one subscribed to the hub is served there.

pub use cc_screen_push::*;

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use crate::engine::AppState;

/// How often we sweep the session list for busy→waiting transitions.
const TICK: Duration = Duration::from_secs(2);

/// Background sweep: on each tick, flag any session that has just crossed from
/// working into waiting (`false → true`) and buzz every device. The first time
/// we see a session we only record its state (no buzz) so already-idle sessions
/// at startup don't all fire at once.
pub async fn finish_watcher(state: AppState) {
    let mut prev: HashMap<String, bool> = HashMap::new();
    let mut interval = tokio::time::interval(TICK);
    loop {
        interval.tick().await;
        let mut seen = HashSet::new();
        for s in state.list() {
            seen.insert(s.name.clone());
            let waiting = s.waiting();
            let was = prev.insert(s.name.clone(), waiting);
            if was == Some(false) && waiting {
                let title = format!("{} is waiting", s.short);
                let preview = s.preview();
                let body = if preview.is_empty() { "finished — tap to open".to_string() } else { preview };
                state.inner.push.notify(&title, &body, &s.name).await;
            }
        }
        prev.retain(|name, _| seen.contains(name));
    }
}

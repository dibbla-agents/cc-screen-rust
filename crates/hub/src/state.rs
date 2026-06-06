//! Shared hub state (the axum `State`): the agent registry, the per-agent uplink
//! tokens, and the client-facing auth gate.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use cc_screen_auth::Auth;

use crate::registry::Registry;

#[derive(Clone)]
pub struct HubState {
    pub registry: Registry,
    /// `machine_id → uplink token`. Empty = open mode (tailnet/dev): any agent may
    /// register. Non-empty = each machine must present its configured token.
    pub agent_tokens: Arc<HashMap<String, String>>,
    /// The client-facing gate (browser cookie / `ccs` bearer). Independent of the
    /// per-agent uplink tokens above.
    pub client_auth: Auth,
    /// Origin/Host validation policy (anti cross-origin / DNS-rebinding), enforced
    /// independent of the client-auth gate. See `handlers::require_client_auth`.
    pub origin: cc_screen_auth::OriginPolicy,
    /// Login attempt throttle (per-source backoff/lockout).
    pub login_throttle: std::sync::Arc<cc_screen_auth::LoginThrottle>,
    /// The hub's own config dir, for hub-local state (favorites, push subs).
    pub config_dir: PathBuf,
    /// Centralized Web Push: one VAPID keypair + subscription store for the whole
    /// fleet (a phone gets one subscription regardless of how many machines).
    pub push: Arc<cc_screen_push::Push>,
    /// In-flight bulk transfers (download/upload/clip) over the dedicated WS.
    pub bulk: crate::bulk::BulkRegistry,
}

impl HubState {
    /// Cheap pre-upgrade check: in configured mode the presented token must match
    /// *some* agent's token (the exact `(machine, token)` pairing is verified
    /// after `Register`, once the machine_id is known).
    pub fn handshake_token_plausible(&self, presented: Option<&str>) -> bool {
        if self.agent_tokens.is_empty() {
            return true; // open mode
        }
        match presented {
            Some(t) => self.agent_tokens.values().any(|v| v == t),
            None => false,
        }
    }

    /// Post-`Register` check that this `(machine_id, token)` pair is authorized.
    /// Open mode (no tokens configured) accepts any machine; configured mode
    /// accepts only a known machine presenting its exact token.
    pub fn uplink_token_ok_for(&self, machine_id: &str, presented: Option<&str>) -> bool {
        match self.agent_tokens.get(machine_id) {
            None => self.agent_tokens.is_empty(),
            Some(expected) => presented == Some(expected.as_str()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(tokens: &[(&str, &str)]) -> HubState {
        let map: HashMap<String, String> =
            tokens.iter().map(|(m, t)| (m.to_string(), t.to_string())).collect();
        HubState {
            registry: Registry::new(),
            agent_tokens: Arc::new(map),
            client_auth: Auth::new(None, None, [0u8; 32]),
            origin: cc_screen_auth::OriginPolicy::default(),
            login_throttle: Arc::new(cc_screen_auth::LoginThrottle::new()),
            config_dir: std::env::temp_dir(),
            push: Arc::new(cc_screen_push::Push::new(&std::env::temp_dir())),
            bulk: crate::bulk::BulkRegistry::default(),
        }
    }

    #[test]
    fn open_mode_accepts_any_agent() {
        let s = state(&[]);
        assert!(s.handshake_token_plausible(None));
        assert!(s.handshake_token_plausible(Some("whatever")));
        assert!(s.uplink_token_ok_for("any-machine", None));
        assert!(s.uplink_token_ok_for("any-machine", Some("whatever")));
    }

    #[test]
    fn configured_mode_requires_matching_per_agent_token() {
        let s = state(&[("alpha", "secretA"), ("beta", "secretB")]);
        // Handshake: token must match some configured value.
        assert!(s.handshake_token_plausible(Some("secretA")));
        assert!(!s.handshake_token_plausible(Some("nope")));
        assert!(!s.handshake_token_plausible(None));
        // Post-register: the (machine, token) pair must line up exactly.
        assert!(s.uplink_token_ok_for("alpha", Some("secretA")));
        assert!(!s.uplink_token_ok_for("alpha", Some("secretB")), "wrong machine's token");
        assert!(!s.uplink_token_ok_for("alpha", None));
        // An unknown machine is rejected in configured mode.
        assert!(!s.uplink_token_ok_for("ghost", Some("secretA")));
    }
}

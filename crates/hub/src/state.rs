//! Shared hub state (the axum `State`): the agent registry, the per-agent uplink
//! tokens, and the client-facing auth gate.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use cc_screen_auth::Auth;

use crate::registry::Registry;

/// A tenant identity. Single-tenant installs resolve every agent to
/// [`cc_screen_auth::OWNER`]; a multi-tenant build resolves a real `users.id`.
pub type UserId = String;
/// The hub's durable identity for a registered agent. In static-map
/// (single-tenant) mode this is just the `machine_id`; multi-tenant assigns an
/// `agents.id` row.
pub type AgentId = String;

/// Resolve an inbound uplink `(machine_id, token)` to the `(tenant, agent)` it
/// belongs to, or `None` to reject (proposal 0001 §9.1). Today's static token map
/// and a future Postgres-backed lookup both implement this **one seam**, so the
/// relay's tenant-isolation match (§4.1) runs identically in both modes — in
/// single-tenant the owner is compared to itself (always true), so there is never
/// a separate single-tenant relay path to maintain.
pub trait AgentTokens: Send + Sync {
    fn resolve(&self, machine_id: &str, token: Option<&str>) -> Option<(UserId, AgentId)>;
}

/// The single-tenant resolver: today's `machine_id → token` map. Open mode (empty
/// map) accepts any machine; configured mode accepts only a known machine
/// presenting its exact token. Either way the tenant is always
/// [`cc_screen_auth::OWNER`] and the [`AgentId`] is the `machine_id`. This is the
/// only impl that ships in single-tenant `cc-screen-rust`; the SaaS adds a
/// `PgTokens` alongside it without touching this one.
pub struct StaticMap {
    pub tokens: Arc<HashMap<String, String>>,
}

impl AgentTokens for StaticMap {
    fn resolve(&self, machine_id: &str, token: Option<&str>) -> Option<(UserId, AgentId)> {
        let ok = match self.tokens.get(machine_id) {
            None => self.tokens.is_empty(),
            Some(expected) => token == Some(expected.as_str()),
        };
        ok.then(|| (cc_screen_auth::OWNER.to_string(), machine_id.to_string()))
    }
}

#[derive(Clone)]
pub struct HubState {
    pub registry: Registry,
    /// `machine_id → uplink token`. Empty = open mode (tailnet/dev): any agent may
    /// register. Non-empty = each machine must present its configured token.
    pub agent_tokens: Arc<HashMap<String, String>>,
    /// The explicit `CCHUB_ALLOW_OPEN_UPLINK` opt-in. Only meaningful in open mode
    /// (empty `agent_tokens`): with it unset, the runtime backstop in
    /// `uplink_server::agent_ws` refuses a registration arriving through a reverse
    /// proxy (proposal 0010, Part 3).
    pub allow_open_uplink: bool,
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
    /// Session summarizer (proposal 0022): the single keyholder + spend gate that
    /// answers agents' `SummaryRequest`s by calling Haiku. Shared so the running
    /// budget tally is one place.
    pub summary: Arc<crate::summarizer::Summarizer>,
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

    /// Resolve an authorized `(machine_id, token)` to its `(user_id, agent_id)`
    /// through the [`AgentTokens`] seam (proposal 0001 §9.1) — the point a future
    /// multi-tenant build swaps the static map for a Postgres-backed resolver.
    /// Equivalent today to [`HubState::uplink_token_ok_for`] returning true, but it
    /// also yields the tenant identity the relay match (§4.1) will gate on. Returns
    /// `None` on rejection.
    pub fn resolve_agent(&self, machine_id: &str, token: Option<&str>) -> Option<(UserId, AgentId)> {
        StaticMap { tokens: self.agent_tokens.clone() }.resolve(machine_id, token)
    }

    /// True when the uplink is open (no per-agent tokens) and the operator has NOT
    /// explicitly opted in via `CCHUB_ALLOW_OPEN_UPLINK`. In this state any party
    /// who reaches `/agent/ws` could impersonate any machine, so the runtime
    /// backstop (proposal 0010, Part 3) refuses a registration that arrived through
    /// a reverse proxy (forwarded headers present ⇒ not local).
    pub fn open_uplink_unguarded(&self) -> bool {
        self.agent_tokens.is_empty() && !self.allow_open_uplink
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
            allow_open_uplink: false,
            client_auth: Auth::new(None, None, [0u8; 32]),
            origin: cc_screen_auth::OriginPolicy::default(),
            login_throttle: Arc::new(cc_screen_auth::LoginThrottle::new()),
            config_dir: std::env::temp_dir(),
            push: Arc::new(cc_screen_push::Push::new(&std::env::temp_dir())),
            bulk: crate::bulk::BulkRegistry::default(),
            summary: Arc::new(crate::summarizer::Summarizer::disabled()),
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

    // The §9.1 `AgentTokens` seam must agree with the existing bool check in BOTH
    // modes, and yield OWNER as the tenant + the machine_id as the agent id — so
    // single-tenant gets tenant identity "for free" with no behavior change.
    #[test]
    fn resolve_agent_matches_token_check_and_yields_owner() {
        let open = state(&[]);
        assert_eq!(
            open.resolve_agent("any", Some("whatever")),
            Some((cc_screen_auth::OWNER.to_string(), "any".to_string()))
        );
        assert_eq!(open.resolve_agent("any", None), Some((cc_screen_auth::OWNER.to_string(), "any".to_string())));

        let cfg = state(&[("alpha", "secretA"), ("beta", "secretB")]);
        // Authorized pair ⇒ Some((OWNER, machine)); mirrors uplink_token_ok_for.
        assert_eq!(
            cfg.resolve_agent("alpha", Some("secretA")),
            Some((cc_screen_auth::OWNER.to_string(), "alpha".to_string()))
        );
        // Every rejection the bool check makes, the resolver also rejects.
        for (m, t) in [("alpha", Some("secretB")), ("alpha", None), ("ghost", Some("secretA"))] {
            assert_eq!(cfg.uplink_token_ok_for(m, t), cfg.resolve_agent(m, t).is_some());
            assert!(cfg.resolve_agent(m, t).is_none());
        }
    }
}

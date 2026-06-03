//! Web Push — the "an agent finished its turn, buzz my phone" channel.
//!
//! Tier 1 (shipped in 0.2.1) surfaced the `waiting` state in-app; this is the
//! out-of-band push for when the PWA is closed. Pieces:
//!
//!   - a VAPID P-256 keypair, generated once and persisted in the config dir
//!     (the private scalar never leaves the box); the public point is handed to
//!     the browser as the `applicationServerKey`;
//!   - a store of browser `PushSubscription`s (multi-device);
//!   - `notify`, which builds a VAPID-signed, aes128gcm-encrypted request with
//!     `web-push-native` (pure-Rust RustCrypto + jwt-simple — the `web-push`
//!     crate needs system OpenSSL this box lacks) and POSTs it to each
//!     subscription's endpoint via `ureq` (pure-Rust rustls+ring). Dead
//!     endpoints (404/410) are pruned.
//!
//! The busy→waiting *edge* that triggers a push lives in `finish_watcher`,
//! spawned from `main`. See `engine::IDLE_AFTER_SECS` for the idle threshold.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use serde::{Deserialize, Serialize};

use crate::engine::AppState;

/// How often we sweep the session list for busy→waiting transitions.
const TICK: Duration = Duration::from_secs(2);
/// VAPID `sub` claim — an identifier for the sender (push services may use it to
/// contact the operator about delivery problems).
const VAPID_CONTACT: &str = "mailto:erik@dibbla.com";

static FILE_LOCK: Mutex<()> = Mutex::new(());

#[derive(Serialize, Deserialize, Clone)]
struct VapidKeys {
    /// The raw 32-byte P-256 private scalar, base64url — rebuilt into a
    /// jwt-simple ES256KeyPair to sign each push's VAPID JWT.
    private_b64: String,
    /// The uncompressed public point (65 bytes), base64url — the browser's
    /// applicationServerKey.
    public_b64: String,
}

/// One browser push subscription (the `PushSubscription` shape, flattened).
#[derive(Serialize, Deserialize, Clone)]
pub struct StoredSub {
    pub endpoint: String,
    pub p256dh: String,
    pub auth: String,
}

pub struct Push {
    config_dir: PathBuf,
    keys: VapidKeys,
    subs: Mutex<Vec<StoredSub>>,
}

impl Push {
    /// Load (or first-time generate) the VAPID keypair and load any stored
    /// subscriptions. Best-effort: a write failure just means a buzz can't reach
    /// that device, never that a session breaks.
    pub fn new(config_dir: &Path) -> Push {
        let keys = load_or_make_keys(config_dir);
        let subs = load_subs(config_dir);
        Push { config_dir: config_dir.to_path_buf(), keys, subs: Mutex::new(subs) }
    }

    /// The base64url application server (public) key the client subscribes with.
    pub fn application_server_key(&self) -> String {
        self.keys.public_b64.clone()
    }

    /// Upsert a subscription by endpoint (re-subscribing the same device is a
    /// no-op rather than a duplicate buzz).
    pub fn add_sub(&self, sub: StoredSub) {
        let mut subs = self.subs.lock().unwrap();
        if let Some(slot) = subs.iter_mut().find(|s| s.endpoint == sub.endpoint) {
            *slot = sub;
        } else {
            subs.push(sub);
        }
        save_subs(&self.config_dir, &subs);
    }

    /// Drop a subscription (the client toggled notifications off).
    pub fn remove_sub(&self, endpoint: &str) {
        let mut subs = self.subs.lock().unwrap();
        let before = subs.len();
        subs.retain(|s| s.endpoint != endpoint);
        if subs.len() != before {
            save_subs(&self.config_dir, &subs);
        }
    }

    fn snapshot(&self) -> Vec<StoredSub> {
        self.subs.lock().unwrap().clone()
    }

    fn prune(&self, dead: &HashSet<String>) {
        let mut subs = self.subs.lock().unwrap();
        let before = subs.len();
        subs.retain(|s| !dead.contains(&s.endpoint));
        if subs.len() != before {
            save_subs(&self.config_dir, &subs);
        }
    }

    /// Fan a notification out to every stored device. Encryption + the blocking
    /// HTTP POSTs run on a blocking thread (ureq is sync); afterwards any
    /// 404/410 ("gone") endpoints are pruned.
    pub async fn notify(&self, title: &str, body: &str, session: &str) {
        let subs = self.snapshot();
        if subs.is_empty() {
            return;
        }
        let priv_b64 = self.keys.private_b64.clone();
        let payload = serde_json::to_vec(&serde_json::json!({
            "title": title,
            "body": body,
            "session": session,
            "tag": session,
        }))
        .unwrap_or_default();

        let dead = tokio::task::spawn_blocking(move || send_all(&priv_b64, &subs, &payload))
            .await
            .unwrap_or_default();
        if !dead.is_empty() {
            self.prune(&dead);
        }
    }
}

enum SendErr {
    /// 404/410 — the subscription is gone; prune it.
    Gone,
    Other(String),
}

fn send_all(priv_b64: &str, subs: &[StoredSub], payload: &[u8]) -> HashSet<String> {
    let mut dead = HashSet::new();
    for s in subs {
        match send_one(priv_b64, s, payload) {
            Ok(()) => {}
            Err(SendErr::Gone) => {
                dead.insert(s.endpoint.clone());
            }
            Err(SendErr::Other(e)) => {
                tracing::warn!("push: send to {} failed: {e}", short_endpoint(&s.endpoint));
            }
        }
    }
    dead
}

fn send_one(priv_b64: &str, sub: &StoredSub, payload: &[u8]) -> Result<(), SendErr> {
    use web_push_native::jwt_simple::algorithms::ES256KeyPair;
    use web_push_native::{Auth, WebPushBuilder};

    let oops = |what: &str, e: String| SendErr::Other(format!("{what}: {e}"));

    // Rebuild the VAPID signing key from the stored private scalar.
    let scalar = URL_SAFE_NO_PAD.decode(priv_b64).map_err(|e| oops("vapid key", e.to_string()))?;
    let kp = ES256KeyPair::from_bytes(&scalar).map_err(|e| oops("vapid key", e.to_string()))?;

    // The device's public key (p256dh, uncompressed SEC1) and auth secret.
    let ua_bytes = URL_SAFE_NO_PAD.decode(&sub.p256dh).map_err(|e| oops("p256dh", e.to_string()))?;
    let ua_public =
        p256::PublicKey::from_sec1_bytes(&ua_bytes).map_err(|e| oops("p256dh", e.to_string()))?;
    let auth_bytes = URL_SAFE_NO_PAD.decode(&sub.auth).map_err(|e| oops("auth", e.to_string()))?;
    if auth_bytes.len() != 16 {
        return Err(SendErr::Other("auth secret must be 16 bytes".into()));
    }
    let auth = Auth::clone_from_slice(&auth_bytes);

    let uri = sub
        .endpoint
        .parse::<http::Uri>()
        .map_err(|e| oops("endpoint", e.to_string()))?;

    // web-push-native does the RFC8291 encryption + VAPID JWT and hands back a
    // ready http::Request (headers: TTL, Content-Encoding aes128gcm, Authorization).
    let request = WebPushBuilder::new(uri, ua_public, auth)
        .with_vapid(&kp, VAPID_CONTACT)
        .build(payload.to_vec())
        .map_err(|e| oops("encrypt", e.to_string()))?;

    // Replay it as a ureq POST (pure-Rust TLS). ureq sets Content-Length itself.
    let mut req = ureq::request(request.method().as_str(), &request.uri().to_string());
    for (name, value) in request.headers() {
        if name.as_str().eq_ignore_ascii_case("content-length") {
            continue;
        }
        if let Ok(v) = value.to_str() {
            req = req.set(name.as_str(), v);
        }
    }
    match req.send_bytes(&request.into_body()) {
        Ok(_) => Ok(()),
        Err(ureq::Error::Status(404, _)) | Err(ureq::Error::Status(410, _)) => Err(SendErr::Gone),
        Err(ureq::Error::Status(code, _)) => Err(SendErr::Other(format!("status {code}"))),
        Err(ureq::Error::Transport(t)) => Err(SendErr::Other(t.to_string())),
    }
}

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

// ── Persistence (atomic temp+rename, mirroring manifest.rs) ───────────────────

fn keys_file(dir: &Path) -> PathBuf {
    dir.join("vapid.json")
}
fn subs_file(dir: &Path) -> PathBuf {
    dir.join("push-subscriptions.json")
}

fn load_or_make_keys(dir: &Path) -> VapidKeys {
    let _g = FILE_LOCK.lock().unwrap();
    if let Some(k) = std::fs::read_to_string(keys_file(dir))
        .ok()
        .and_then(|s| serde_json::from_str::<VapidKeys>(&s).ok())
    {
        return k;
    }
    let keys = generate_keys();
    write_atomic(&keys_file(dir), &serde_json::to_vec_pretty(&keys).unwrap_or_default());
    tracing::info!("push: generated a new VAPID keypair");
    keys
}

fn load_subs(dir: &Path) -> Vec<StoredSub> {
    let _g = FILE_LOCK.lock().unwrap();
    std::fs::read_to_string(subs_file(dir))
        .ok()
        .and_then(|s| serde_json::from_str::<Vec<StoredSub>>(&s).ok())
        .unwrap_or_default()
}

fn save_subs(dir: &Path, subs: &[StoredSub]) {
    let _g = FILE_LOCK.lock().unwrap();
    if let Ok(b) = serde_json::to_vec_pretty(subs) {
        write_atomic(&subs_file(dir), &b);
    }
}

fn write_atomic(path: &Path, bytes: &[u8]) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let tmp = path.with_extension("json.tmp");
    if std::fs::write(&tmp, bytes).is_ok() {
        let _ = std::fs::rename(&tmp, path);
    }
}

/// Generate a fresh VAPID P-256 keypair: the raw private scalar (for signing) and
/// the uncompressed public point as base64url (the browser's applicationServerKey).
fn generate_keys() -> VapidKeys {
    use p256::elliptic_curve::sec1::ToEncodedPoint;
    let sk = p256::SecretKey::random(&mut rand_core::OsRng);
    let public = sk.public_key().to_encoded_point(false); // false = uncompressed (65 bytes)
    VapidKeys {
        private_b64: URL_SAFE_NO_PAD.encode(sk.to_bytes()),
        public_b64: URL_SAFE_NO_PAD.encode(public.as_bytes()),
    }
}

fn short_endpoint(endpoint: &str) -> &str {
    // Just the host for logs — endpoints carry a long opaque device token.
    endpoint
        .split_once("://")
        .map(|(_, rest)| rest.split('/').next().unwrap_or(rest))
        .unwrap_or(endpoint)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_key_roundtrips() {
        let k = generate_keys();
        // Uncompressed P-256 point = 65 bytes (0x04 ‖ X ‖ Y).
        let pubk = URL_SAFE_NO_PAD.decode(&k.public_b64).unwrap();
        assert_eq!(pubk.len(), 65, "expected 65-byte uncompressed point");
        assert_eq!(pubk[0], 0x04, "uncompressed point marker");
        // The stored 32-byte scalar must rebuild into a usable ES256 signing key.
        let scalar = URL_SAFE_NO_PAD.decode(&k.private_b64).unwrap();
        assert_eq!(scalar.len(), 32, "expected 32-byte private scalar");
        use web_push_native::jwt_simple::algorithms::ES256KeyPair;
        assert!(ES256KeyPair::from_bytes(&scalar).is_ok(), "scalar must rebuild an ES256 key");
    }

    #[test]
    fn keys_persist_and_reload() {
        let dir = std::env::temp_dir().join(format!("ccr-push-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let a = load_or_make_keys(&dir);
        let b = load_or_make_keys(&dir); // second call must reuse, not regenerate
        assert_eq!(a.public_b64, b.public_b64);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn subs_upsert_and_remove() {
        let dir = std::env::temp_dir().join(format!("ccr-subs-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let push = Push::new(&dir);
        let sub = |e: &str| StoredSub { endpoint: e.into(), p256dh: "p".into(), auth: "a".into() };
        push.add_sub(sub("https://push.example/a"));
        push.add_sub(sub("https://push.example/a")); // upsert, not dup
        push.add_sub(sub("https://push.example/b"));
        assert_eq!(push.snapshot().len(), 2);
        push.remove_sub("https://push.example/a");
        let left = push.snapshot();
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].endpoint, "https://push.example/b");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

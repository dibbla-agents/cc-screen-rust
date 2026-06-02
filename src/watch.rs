// Real-time filesystem watching for the editor's file tree + open file.
//
// One debounced inotify watcher (the `notify` crate) backs every connected
// /api/watch client. Clients subscribe to the DIRECTORIES they have on screen —
// expanded tree folders + the open file's parent — and we watch each such dir
// NON-recursively, so the watch count tracks what's visible rather than a whole
// repo (which would blow past the inotify limit). Every watch path is confined
// to $HOME via confine::resolve_under, exactly like the file endpoints.
//
// A directory watch reports both entry changes (create/delete/rename → tree
// deltas) and content changes of files directly in it (modify → the open file),
// so the directory is the single subscription unit. We push a coalesced
// `{t:"fs", dir, paths}` event to each subscriber; the client re-reads truth
// (refresh the listing / re-read the file), so the events are deliberately
// coarse — no fragile delta application.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        State,
    },
    response::Response,
};
use futures_util::{SinkExt, StreamExt};
use notify_debouncer_mini::{
    new_debouncer,
    notify::{RecommendedWatcher, RecursiveMode},
    DebounceEventResult, Debouncer,
};
use serde::Deserialize;
use serde_json::json;
use tokio::sync::mpsc;

use crate::confine::resolve_under;
use crate::engine::AppState;

// Don't let one client pin an unbounded number of inotify watches.
const MAX_DIRS_PER_CLIENT: usize = 512;
// Coalesce event bursts (an agent/editor writes a file in several syscalls).
const DEBOUNCE: Duration = Duration::from_millis(200);

/// A coalesced change in one watched directory.
pub struct FsEvent {
    pub dir: String,
    pub paths: Vec<String>,
}

#[derive(Default)]
struct Subs {
    by_dir: HashMap<PathBuf, HashSet<u64>>,
    by_client: HashMap<u64, HashSet<PathBuf>>,
}

struct WatchState {
    subs: Mutex<Subs>,
    clients: Mutex<HashMap<u64, mpsc::UnboundedSender<FsEvent>>>,
}

/// The shared filesystem watcher held in AppState. Methods take `&self`; the
/// internal maps + the debouncer are each behind their own lock.
pub struct Watcher {
    state: Arc<WatchState>,
    debouncer: Mutex<Debouncer<RecommendedWatcher>>,
    next_id: AtomicU64,
    home: PathBuf,
}

impl Watcher {
    pub fn new(home: PathBuf) -> Watcher {
        let state = Arc::new(WatchState {
            subs: Mutex::new(Subs::default()),
            clients: Mutex::new(HashMap::new()),
        });
        let cb_state = state.clone();
        let debouncer = new_debouncer(DEBOUNCE, move |res: DebounceEventResult| {
            if let Ok(events) = res {
                route(&cb_state, events.into_iter().map(|e| e.path).collect());
            }
        })
        .expect("create filesystem watcher");
        Watcher {
            state,
            debouncer: Mutex::new(debouncer),
            next_id: AtomicU64::new(1),
            home,
        }
    }

    /// Register a connected client and hand back its id + event receiver.
    fn register(&self) -> (u64, mpsc::UnboundedReceiver<FsEvent>) {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::unbounded_channel();
        self.state.clients.lock().unwrap().insert(id, tx);
        (id, rx)
    }

    /// Drop a client on disconnect: forget its channel and all its subscriptions,
    /// unwatching any directory that loses its last subscriber.
    fn unregister(&self, id: u64) {
        self.state.clients.lock().unwrap().remove(&id);
        let orphans = {
            let mut subs = self.state.subs.lock().unwrap();
            let Subs { by_dir, by_client } = &mut *subs;
            let mut orphans = Vec::new();
            for dir in by_client.remove(&id).unwrap_or_default() {
                if let Some(set) = by_dir.get_mut(&dir) {
                    set.remove(&id);
                    if set.is_empty() {
                        by_dir.remove(&dir);
                        orphans.push(dir);
                    }
                }
            }
            orphans
        };
        let mut deb = self.debouncer.lock().unwrap();
        for dir in orphans {
            let _ = deb.watcher().unwatch(&dir);
        }
    }

    /// Subscribe `id` to a directory (confined to $HOME). Watches it on the first
    /// subscriber; ignores paths outside $HOME, non-directories, and any beyond
    /// the per-client cap.
    fn subscribe(&self, id: u64, raw: &str) {
        let dir = match resolve_under(&self.home, raw) {
            Some(d) => d,
            None => return,
        };
        if !dir.is_dir() {
            return;
        }
        let first = {
            let mut subs = self.state.subs.lock().unwrap();
            let Subs { by_dir, by_client } = &mut *subs;
            let owned = by_client.entry(id).or_default();
            if !owned.contains(&dir) && owned.len() >= MAX_DIRS_PER_CLIENT {
                tracing::debug!("watch: client {id} hit the {MAX_DIRS_PER_CLIENT}-dir cap");
                return;
            }
            owned.insert(dir.clone());
            let set = by_dir.entry(dir.clone()).or_default();
            let first = set.is_empty();
            set.insert(id);
            first
        };
        if first {
            if let Err(e) = self
                .debouncer
                .lock()
                .unwrap()
                .watcher()
                .watch(&dir, RecursiveMode::NonRecursive)
            {
                tracing::debug!("watch {}: {e}", dir.display());
            }
        }
    }

    /// Unsubscribe `id`; unwatch when the directory loses its last subscriber.
    fn unsubscribe(&self, id: u64, raw: &str) {
        let dir = match resolve_under(&self.home, raw) {
            Some(d) => d,
            None => return,
        };
        let now_empty = {
            let mut subs = self.state.subs.lock().unwrap();
            let Subs { by_dir, by_client } = &mut *subs;
            if let Some(owned) = by_client.get_mut(&id) {
                owned.remove(&dir);
            }
            match by_dir.get_mut(&dir) {
                Some(set) => {
                    set.remove(&id);
                    if set.is_empty() {
                        by_dir.remove(&dir);
                        true
                    } else {
                        false
                    }
                }
                None => false,
            }
        };
        if now_empty {
            let _ = self.debouncer.lock().unwrap().watcher().unwatch(&dir);
        }
    }
}

/// Route a debounced batch of changed paths to subscribers, grouped by the
/// parent directory (the unit clients watch).
fn route(state: &WatchState, paths: Vec<PathBuf>) {
    let mut by_dir: HashMap<PathBuf, Vec<String>> = HashMap::new();
    for p in paths {
        if let Some(parent) = p.parent() {
            by_dir
                .entry(parent.to_path_buf())
                .or_default()
                .push(p.to_string_lossy().into_owned());
        }
    }
    let subs = state.subs.lock().unwrap();
    let clients = state.clients.lock().unwrap();
    for (dir, paths) in by_dir {
        let Some(ids) = subs.by_dir.get(&dir) else {
            continue;
        };
        let dir_s = dir.to_string_lossy().into_owned();
        for id in ids {
            if let Some(tx) = clients.get(id) {
                let _ = tx.send(FsEvent {
                    dir: dir_s.clone(),
                    paths: paths.clone(),
                });
            }
        }
    }
}

#[derive(Deserialize)]
struct WatchFrame {
    t: String,
    #[serde(default)]
    dirs: Vec<String>,
}

// ── GET /api/watch (WebSocket) ───────────────────────────────────────────────
pub async fn watch_ws(State(app): State<AppState>, ws: WebSocketUpgrade) -> Response {
    ws.on_upgrade(move |socket| handle(socket, app))
}

async fn handle(socket: WebSocket, app: AppState) {
    let (mut sink, mut stream) = socket.split();
    let (id, mut rx) = app.inner.watcher.register();

    // Forward filesystem events to the client as JSON text frames.
    let mut send_task = tokio::spawn(async move {
        while let Some(ev) = rx.recv().await {
            let msg = json!({ "t": "fs", "dir": ev.dir, "paths": ev.paths }).to_string();
            if sink.send(Message::Text(msg)).await.is_err() {
                break;
            }
        }
    });

    // Apply sub/unsub frames as the visible directory set changes.
    let app2 = app.clone();
    let mut recv_task = tokio::spawn(async move {
        while let Some(Ok(msg)) = stream.next().await {
            match msg {
                Message::Text(t) => {
                    if let Ok(f) = serde_json::from_str::<WatchFrame>(&t) {
                        match f.t.as_str() {
                            "sub" => f.dirs.iter().for_each(|d| app2.inner.watcher.subscribe(id, d)),
                            "unsub" => f.dirs.iter().for_each(|d| app2.inner.watcher.unsubscribe(id, d)),
                            _ => {}
                        }
                    }
                }
                Message::Close(_) => break,
                _ => {}
            }
        }
    });

    tokio::select! {
        _ = &mut send_task => recv_task.abort(),
        _ = &mut recv_task => send_task.abort(),
    }
    app.inner.watcher.unregister(id);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn detects_a_create_in_a_watched_dir() {
        // Unique temp dir; it is its own confinement root.
        let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let dir = std::env::temp_dir().join(format!("ccwatch-{nanos}"));
        std::fs::create_dir_all(&dir).unwrap();

        let w = Watcher::new(dir.clone());
        let (id, mut rx) = w.register();
        w.subscribe(id, dir.to_str().unwrap());

        std::fs::write(dir.join("hello.txt"), b"hi").unwrap();

        // Wait up to ~2s for the debounced event.
        let mut got = None;
        for _ in 0..40 {
            if let Ok(ev) = rx.try_recv() {
                got = Some(ev);
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = std::fs::remove_dir_all(&dir);

        let ev = got.expect("expected a filesystem event");
        assert_eq!(ev.dir, dir.to_string_lossy());
        assert!(ev.paths.iter().any(|p| p.ends_with("hello.txt")), "paths: {:?}", ev.paths);
    }

    #[test]
    fn ignores_paths_outside_home() {
        let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let home = std::env::temp_dir().join(format!("ccwatch-home-{nanos}"));
        std::fs::create_dir_all(&home).unwrap();
        let w = Watcher::new(home.clone());
        let (id, _rx) = w.register();
        // Outside the confinement root → silently ignored (no panic, no watch).
        w.subscribe(id, "/etc");
        let subs = w.state.subs.lock().unwrap();
        assert!(subs.by_dir.is_empty(), "should not watch outside home");
        drop(subs);
        let _ = std::fs::remove_dir_all(&home);
    }
}

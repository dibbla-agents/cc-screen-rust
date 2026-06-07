//! Recursive, $HOME-confined directory search for the create-session flow
//! (proposal 0016). A bounded breadth-first walk under a root, fuzzy-ranked so
//! the obvious project (a shallow basename match that was recently modified or
//! is already a live session's cwd) floats to the top. Shared by the REST
//! handler (`files.rs`) and the hub-relayed op (`fileops.rs`); both wrap these
//! results in the JSON the PWA expects.
//!
//! Confinement reuses the same boundary as `/api/dirs`: the caller resolves
//! `root` through `resolve_existing_under(&home, …)` first, and every hit is
//! re-canonicalized and re-checked to stay within `$HOME` — so the search can
//! never surface a path the one-level browser couldn't already reach (symlinks
//! pointing outside home are dropped). Symlinked directories pointing *inside*
//! home are followed, so an isolated symlink-farm `$HOME` still surfaces its
//! real projects.

use std::collections::{HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

/// One ranked directory hit.
pub struct DirHit {
    pub path: String,
    pub name: String,
    pub rel: String,
    pub depth: usize,
    pub score: i64,
    pub mtime: i64,
}

// Hard bounds so a large $HOME can't hang the walk. These are a deliberate cap,
// not a silent truncation — results are ranked and the best matches survive the
// ~200-result cap; the client also debounces, so a keystroke never fans out
// more than one of these at a time.
const MAX_DEPTH: usize = 8;
const MAX_VISITED: usize = 40_000;
const MAX_RESULTS: usize = 200;
const TIME_BUDGET_MS: u128 = 150;

// Heavy / noisy directories that are never descended and never returned. Hidden
// (dot) directories are pruned separately.
const PRUNE: &[&str] = &[
    "node_modules",
    ".git",
    "target",
    ".cache",
    "dist",
    "build",
    "vendor",
    ".venv",
    "venv",
    "__pycache__",
    ".next",
    ".turbo",
    ".gradle",
];

fn mtime_secs(meta: &std::fs::Metadata) -> i64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Home-relative display path with a `~` prefix (e.g. `~/development/foo`).
/// Falls back to the absolute path if it doesn't sit under home.
fn rel_home(home: &Path, p: &Path) -> String {
    if p == home {
        return "~".to_string();
    }
    match p.strip_prefix(home) {
        Ok(rest) => format!("~/{}", rest.to_string_lossy()),
        Err(_) => p.to_string_lossy().into_owned(),
    }
}

/// Case-insensitive subsequence match of `needle` (already lowercased) against
/// `hay`. `None` when it isn't a subsequence; a higher score is a better match
/// (contiguous runs, word-start hits, and a head-of-string match all add).
fn fuzzy(needle: &[char], hay: &str) -> Option<i64> {
    if needle.is_empty() {
        return Some(0);
    }
    let h: Vec<char> = hay.chars().flat_map(|c| c.to_lowercase()).collect();
    let mut hi = 0usize;
    let mut score = 0i64;
    let mut last: Option<usize> = None;
    let mut run = 0i64;
    for &nc in needle {
        let mut found = None;
        let mut j = hi;
        while j < h.len() {
            if h[j] == nc {
                found = Some(j);
                break;
            }
            j += 1;
        }
        let pos = found?;
        score += 2;
        if pos == 0 {
            score += 6; // matches at the very start of the string
        }
        if pos == 0 || matches!(h[pos - 1], '/' | '-' | '_' | '.' | ' ') {
            score += 12; // word-start bonus
        }
        if last == Some(pos.wrapping_sub(1)) {
            run += 1;
            score += 4 * run; // contiguous-run bonus, growing
        } else {
            run = 0;
        }
        last = Some(pos);
        hi = pos + 1;
    }
    Some(score)
}

/// Walk the subtree under `root` (already `$HOME`-confined by the caller) and
/// return the directories whose basename or home-relative path fuzzy-match
/// `query`, ranked best-first and capped. `recent` is the set of live/restorable
/// session cwds, which float to the top. Empty query → no results (the client
/// uses `/api/dirs` + a recents shortcut instead).
pub fn search(home: &Path, root: &Path, query: &str, recent: &HashSet<PathBuf>) -> Vec<DirHit> {
    let q = query.trim();
    if q.is_empty() {
        return Vec::new();
    }
    let needle: Vec<char> = q.chars().flat_map(|c| c.to_lowercase()).collect();
    let start = Instant::now();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    let Ok(real_home) = std::fs::canonicalize(home) else {
        return Vec::new();
    };
    let Ok(real_root) = std::fs::canonicalize(root) else {
        return Vec::new();
    };

    // Visited set of *canonical* paths breaks symlink cycles and avoids
    // re-descending a dir reached two ways.
    let mut visited: HashSet<PathBuf> = HashSet::new();
    visited.insert(real_root);
    let mut queue: VecDeque<(PathBuf, usize)> = VecDeque::new();
    queue.push_back((root.to_path_buf(), 0));
    let mut count = 0usize;
    let mut hits: Vec<DirHit> = Vec::new();

    'walk: while let Some((dir, depth)) = queue.pop_front() {
        if start.elapsed().as_millis() > TIME_BUDGET_MS {
            break;
        }
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        let child_depth = depth + 1;
        for ent in rd.flatten() {
            if count >= MAX_VISITED {
                break 'walk;
            }
            count += 1;
            let name = ent.file_name().to_string_lossy().into_owned();
            // Prune heavy + hidden directories: never descended, never returned.
            if name.starts_with('.') || PRUNE.contains(&name.as_str()) {
                continue;
            }
            let path = ent.path();
            // Follow symlinks (metadata, not file_type) so a symlinked dir lists
            // as a folder — matching /api/dirs and keeping symlink-farm homes
            // searchable. Broken links fail metadata() and are skipped.
            let Ok(meta) = std::fs::metadata(&path) else { continue };
            if !meta.is_dir() {
                continue;
            }
            // Re-confine: the real target must stay within $HOME (drops an
            // outward-pointing symlink), and the canonical path dedupes cycles.
            let Ok(canon) = std::fs::canonicalize(&path) else { continue };
            if !(canon == real_home || canon.starts_with(&real_home)) {
                continue;
            }
            if !visited.insert(canon) {
                continue;
            }

            let rel = rel_home(home, &path);
            // Score basename first (the strong signal), else the home-relative
            // path so a query like `dev/foo` still matches across separators.
            let base = fuzzy(&needle, &name);
            let m = match base {
                Some(s) => Some(s + 20), // basename match weighs more than a path-only match
                None => fuzzy(&needle, &rel),
            };
            if let Some(mut score) = m {
                // Shallower is better; the obvious top-level project wins ties.
                score += (MAX_DEPTH.saturating_sub(child_depth)) as i64 * 3;
                let mt = mtime_secs(&meta);
                // Recently-modified bonus, graded by age (≤2d, ≤14d, ≤90d).
                let age = (now - mt).max(0);
                if age <= 2 * 86_400 {
                    score += 12;
                } else if age <= 14 * 86_400 {
                    score += 6;
                } else if age <= 90 * 86_400 {
                    score += 2;
                }
                // Recently-used: this folder is (or hosts) a live/restorable
                // session — a very strong "you want this" signal.
                if recent.contains(&path) {
                    score += 40;
                }
                hits.push(DirHit {
                    path: path.to_string_lossy().into_owned(),
                    name,
                    rel,
                    depth: child_depth,
                    score,
                    mtime: mt,
                });
            }

            if child_depth < MAX_DEPTH {
                queue.push_back((path, child_depth));
            }
        }
    }

    hits.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then(b.mtime.cmp(&a.mtime))
            .then(a.rel.cmp(&b.rel))
    });
    hits.truncate(MAX_RESULTS);
    hits
}

/// Build the JSON response body shared by the REST handler and the hub op.
pub fn results_json(home: &Path, root: &Path, hits: &[DirHit]) -> Value {
    let results: Vec<Value> = hits
        .iter()
        .map(|h| {
            json!({
                "path": h.path,
                "name": h.name,
                "rel": h.rel,
                "depth": h.depth,
                "score": h.score,
                "mtime": h.mtime,
            })
        })
        .collect();
    json!({
        "root": root.to_string_lossy(),
        "home": home.to_string_lossy(),
        "results": results,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn touch_dir(p: &Path) {
        std::fs::create_dir_all(p).unwrap();
    }

    #[test]
    fn finds_deep_match_and_prunes_heavy_dirs() {
        let base = std::env::temp_dir().join(format!("ccr-dirsearch-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let home = base.join("home");
        touch_dir(&home.join("development/cc-screen-rust/src"));
        touch_dir(&home.join("development/cc-screen-saas/docs"));
        touch_dir(&home.join("development/cc-screen-rust/node_modules/cc-screen-deep"));
        touch_dir(&home.join("development/cc-screen-rust/.git/cc-screen-objects"));
        touch_dir(&home.join("misc/unrelated"));

        let recent = HashSet::new();
        let hits = search(&home, &home, "screen", &recent);
        let rels: Vec<&str> = hits.iter().map(|h| h.rel.as_str()).collect();

        assert!(rels.contains(&"~/development/cc-screen-rust"), "got {rels:?}");
        assert!(rels.contains(&"~/development/cc-screen-saas"), "got {rels:?}");
        // Heavy dirs are never descended nor returned.
        assert!(
            !rels.iter().any(|r| r.contains("node_modules") || r.contains(".git")),
            "heavy dirs leaked: {rels:?}"
        );
        // An unrelated folder doesn't match.
        assert!(!rels.iter().any(|r| r.contains("unrelated")), "got {rels:?}");

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn basename_match_ranks_above_path_only_and_recents_win() {
        let base = std::env::temp_dir().join(format!("ccr-dirsearch-rank-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let home = base.join("home");
        touch_dir(&home.join("apple")); // basename match for "app"
        touch_dir(&home.join("zzz/app-helper")); // basename match, deeper
        touch_dir(&home.join("application/nested")); // basename "application"

        let mut recent = HashSet::new();
        recent.insert(home.join("zzz/app-helper"));
        let hits = search(&home, &home, "app", &recent);
        assert!(!hits.is_empty());
        // The recently-used folder should outrank the shallower plain matches.
        assert_eq!(hits[0].rel, "~/zzz/app-helper", "ranking: {:?}", hits.iter().map(|h| (&h.rel, h.score)).collect::<Vec<_>>());

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn empty_query_returns_nothing() {
        let base = std::env::temp_dir().join(format!("ccr-dirsearch-empty-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let home = base.join("home");
        touch_dir(&home.join("a"));
        assert!(search(&home, &home, "", &HashSet::new()).is_empty());
        assert!(search(&home, &home, "   ", &HashSet::new()).is_empty());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[cfg(unix)]
    #[test]
    fn outward_symlink_is_not_followed() {
        use std::os::unix::fs::symlink;
        let base = std::env::temp_dir().join(format!("ccr-dirsearch-sym-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let home = base.join("home");
        let outside = base.join("outside");
        touch_dir(&home);
        touch_dir(&outside.join("secret-screen"));
        // A symlink under home named to match the query, pointing OUTSIDE.
        symlink(&outside, home.join("escape-screen")).unwrap();

        let hits = search(&home, &home, "screen", &HashSet::new());
        // The symlink target sits outside home → its contents must not surface.
        assert!(
            !hits.iter().any(|h| h.rel.contains("secret-screen")),
            "outward symlink leaked: {:?}",
            hits.iter().map(|h| &h.rel).collect::<Vec<_>>()
        );
        let _ = std::fs::remove_dir_all(&base);
    }
}

// Path confinement — the Rust analogue of the Go build's resolveUnderHome /
// resolveUnderRoot / safeRel. Every filesystem endpoint (browse, editor, upload)
// is confined to $HOME (or, for a terminal-pane upload, the session cwd). We
// clean paths lexically (like Go's filepath.Clean — no symlink resolution) and
// check containment component-wise via Path::starts_with, which is stricter than
// a string prefix (so `/home/erik2` never counts as inside `/home/erik`).

use std::path::{Component, Path, PathBuf};

/// Lexically clean a path: fold `.`/`..`, matching filepath.Clean. A relative
/// path keeps leading `..` segments (so `safe_rel` can reject them); an absolute
/// path drops `..` at the root.
pub fn clean(p: &str) -> PathBuf {
    let path = Path::new(p);
    let is_abs = path.is_absolute();
    let mut stack: Vec<Component> = Vec::new();
    for comp in path.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => match stack.last() {
                Some(Component::Normal(_)) => {
                    stack.pop();
                }
                Some(Component::RootDir) => {} // /.. stays /
                _ => {
                    if !is_abs {
                        stack.push(Component::ParentDir);
                    }
                }
            },
            c => stack.push(c),
        }
    }
    let mut out = PathBuf::new();
    for c in stack {
        out.push(c.as_os_str());
    }
    if out.as_os_str().is_empty() {
        out.push(".");
    }
    out
}

/// Clean `p` and confirm it stays within `root` (inclusive). Empty `p` => root.
/// None means "outside the root" (or a non-absolute input).
pub fn resolve_under(root: &Path, p: &str) -> Option<PathBuf> {
    if p.trim().is_empty() {
        return Some(root.to_path_buf());
    }
    let abs = clean(p);
    if !abs.is_absolute() {
        return None;
    }
    if abs == root || abs.starts_with(root) {
        Some(abs)
    } else {
        None
    }
}

/// True when `real` (an already-canonical path) is contained in `real_root`
/// (also canonical), inclusive.
fn contained(real: &Path, real_root: &Path) -> bool {
    real == real_root || real.starts_with(real_root)
}

/// Why a path failed to resolve under a confinement root.
///
/// The split exists so the file plane can answer `404` for a path that is inside
/// the caller's own root but no longer there, instead of a `403` naming
/// confinement for an ordinary `mv`. It is deliberately **asymmetric**:
///
/// > `Missing` is only ever reported for a path whose nearest existing ancestor
/// > is inside the root. Anything outside the root — lexically, after symlink
/// > resolution, or by having no contained ancestor — stays a single,
/// > undifferentiated `Outside`, whether it exists or not.
///
/// That clause is load-bearing, not decoration: `canonicalize` fails before
/// containment is ever checked, so mapping "canonicalize failed" straight to
/// `Missing` would make `$HOME/escape/x` (where `escape -> /etc`) answer
/// *missing* when `/etc/x` is absent and *outside* when it is present — a
/// per-path existence oracle for the whole filesystem, reachable through any
/// symlink an owner left in a shared folder. `symlink_outward_never_discloses`
/// in this module's tests is what keeps it closed.
///
/// **Deliberate exceptions.** Resolution performed *before* authorization —
/// `Cmd::ResolveFolder` and the folder-grant `canonical_under` / `root_for`
/// helpers (proposal 0074) — must answer one undifferentiated refusal, because
/// the caller may hold no grant and must not learn the shape of a filesystem it
/// is not entitled to. Those stay `Option`-shaped on purpose; do not "fix" them
/// to use this enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resolved {
    /// The path escapes the root — lexically, after symlink resolution, or by
    /// having no existing ancestor inside it. Existence is NEVER disclosed for
    /// these: a caller must not learn what does or does not exist outside the
    /// root they hold.
    Outside,
    /// The path is inside the root but is not there (or is unreadable).
    Missing,
}

/// Resolve a path that must already EXIST and stay within `root` **even after
/// symlink resolution**. Lexical clean + lexical containment first (a cheap
/// reject of obvious traversal / non-absolute input), then canonicalize the
/// target and assert real-path containment — so a symlink under the root pointing
/// outside it is rejected, while a symlink whose target stays inside is allowed.
///
/// Returns the *lexical* path (for display + the subsequent fs op); the op
/// re-follows the symlink to the target we just verified is inside the root.
///
/// On failure the reason is typed — see [`Resolved`] for the asymmetry rule that
/// keeps `Missing` from becoming an existence oracle.
pub fn try_resolve_existing_under(root: &Path, p: &str) -> Result<PathBuf, Resolved> {
    let lexical = resolve_under(root, p).ok_or(Resolved::Outside)?;
    // A root that doesn't canonicalize is a $HOME misconfiguration, not a
    // per-path fact — it must never become a probe.
    let real_root = std::fs::canonicalize(root).map_err(|_| Resolved::Outside)?;
    match std::fs::canonicalize(&lexical) {
        Ok(real) => {
            if contained(&real, &real_root) {
                Ok(lexical)
            } else {
                Err(Resolved::Outside)
            }
        }
        // The leaf isn't there (or an intermediate component is unreadable, or a
        // symlink dangles). Only call it Missing once we've proven the nearest
        // ancestor that DOES exist is inside the root.
        Err(_) => {
            let mut anc = lexical.as_path();
            loop {
                anc = match anc.parent() {
                    Some(a) => a,
                    None => return Err(Resolved::Outside),
                };
                if !anc.exists() {
                    continue;
                }
                let real_anc = std::fs::canonicalize(anc).map_err(|_| Resolved::Outside)?;
                return if contained(&real_anc, &real_root) {
                    Err(Resolved::Missing)
                } else {
                    Err(Resolved::Outside)
                };
            }
        }
    }
}

/// [`try_resolve_existing_under`] for callers that don't care *why* it failed.
pub fn resolve_existing_under(root: &Path, p: &str) -> Option<PathBuf> {
    try_resolve_existing_under(root, p).ok()
}

/// Resolve a path for CREATION (the leaf — and possibly intermediate dirs — may
/// not exist yet). Lexical containment first, then canonicalize the **nearest
/// existing ancestor** and assert it stays within `root`. Components that don't
/// exist yet are lexical (no symlink to follow), so this both allows creating a
/// fresh nested tree AND rejects a path whose existing portion (e.g. a symlinked
/// directory) resolves outside the root — before any `create_dir_all` runs.
/// Returns the lexical target path.
pub fn resolve_create_under(root: &Path, p: &str) -> Option<PathBuf> {
    let lexical = resolve_under(root, p)?;
    let real_root = std::fs::canonicalize(root).ok()?;
    let mut anc = lexical.as_path();
    loop {
        anc = anc.parent()?;
        if anc.exists() {
            let real_anc = std::fs::canonicalize(anc).ok()?;
            return contained(&real_anc, &real_root).then_some(lexical);
        }
    }
}

/// Atomically write `content` to `path` via a unique, **unpredictable** temp file
/// in `path`'s parent (mode `0600` on Unix), then rename over `path`. Replaces the
/// old predictable `*.ccwtmp` that two concurrent writes could collide on and that
/// inherited the process umask. The rename also swaps a symlink leaf for a regular
/// file, so a write never follows a symlink out of the confinement root. An
/// existing regular file's permissions are preserved; a brand-new file stays
/// private (`0600`).
#[cfg(unix)]
pub fn atomic_write(path: &Path, content: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let parent = path
        .parent()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "no parent"))?;
    let keep_mode = std::fs::metadata(path)
        .ok()
        .filter(|m| m.is_file())
        .map(|m| m.permissions().mode() & 0o777);
    let tmp = parent.join(format!(".ccw-{}.tmp", &cc_screen_auth::generate_token()[..16]));
    {
        let mut f = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&tmp)?;
        if let Err(e) = f.write_all(content).and_then(|_| f.flush()) {
            let _ = std::fs::remove_file(&tmp);
            return Err(e);
        }
    }
    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    if let Some(mode) = keep_mode {
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode));
    }
    Ok(())
}

#[cfg(not(unix))]
pub fn atomic_write(path: &Path, content: &[u8]) -> std::io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "no parent"))?;
    let tmp = parent.join(format!(".ccw-{}.tmp", &cc_screen_auth::generate_token()[..16]));
    std::fs::write(&tmp, content)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Validate a multipart part's relative filename for upload: reject absolute
/// paths, backslashes, and any leading `..` that would escape the destination.
/// Returns the cleaned relative path.
pub fn safe_rel(name: &str) -> Option<PathBuf> {
    let name = name.trim();
    if name.is_empty() || name.contains('\\') {
        return None;
    }
    let cleaned = clean(name);
    if cleaned.is_absolute() {
        return None;
    }
    if matches!(cleaned.components().next(), Some(Component::ParentDir)) {
        return None;
    }
    Some(cleaned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn confinement() {
        let home = Path::new("/home/u");
        assert_eq!(resolve_under(home, "/home/u/proj"), Some(PathBuf::from("/home/u/proj")));
        assert_eq!(resolve_under(home, ""), Some(home.to_path_buf()));
        assert_eq!(resolve_under(home, "/home/u/../u/x"), Some(PathBuf::from("/home/u/x")));
        assert_eq!(resolve_under(home, "/etc/passwd"), None);
        assert_eq!(resolve_under(home, "/home/u2"), None); // sibling, not inside
        assert_eq!(resolve_under(home, "relative"), None);
        assert_eq!(resolve_under(home, "/home/u/../../etc"), None); // escapes
    }

    #[cfg(unix)]
    #[test]
    fn symlink_safe_resolution() {
        use std::os::unix::fs::symlink;
        let base = std::env::temp_dir().join(format!("ccr-confine-sym-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let home = base.join("home");
        let outside = base.join("outside");
        std::fs::create_dir_all(home.join("real")).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(home.join("real/in.txt"), b"in").unwrap();
        std::fs::write(outside.join("secret.txt"), b"x").unwrap();

        // A symlink under home pointing OUTSIDE.
        symlink(&outside, home.join("escape")).unwrap();
        // A symlink under home pointing back INSIDE.
        symlink(home.join("real"), home.join("inlink")).unwrap();

        // Reading through the outward symlink is rejected; through the inward one,
        // and to a normal in-home file, allowed.
        assert!(resolve_existing_under(&home, &home.join("escape/secret.txt").to_string_lossy()).is_none());
        assert!(resolve_existing_under(&home, &home.join("inlink/in.txt").to_string_lossy()).is_some());
        assert!(resolve_existing_under(&home, &home.join("real/in.txt").to_string_lossy()).is_some());

        // Creating through the outward symlink is rejected; a new file in a real
        // in-home dir is allowed.
        assert!(resolve_create_under(&home, &home.join("escape/new.txt").to_string_lossy()).is_none());
        assert!(resolve_create_under(&home, &home.join("real/new.txt").to_string_lossy()).is_some());

        let _ = std::fs::remove_dir_all(&base);
    }

    /// A path inside the root that simply isn't there is `Missing` (→ 404);
    /// everything outside stays `Outside` (→ 403).
    #[cfg(unix)]
    #[test]
    fn typed_resolution_distinguishes_missing_from_outside() {
        let base = std::env::temp_dir().join(format!("ccr-confine-typed-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let home = base.join("home");
        std::fs::create_dir_all(home.join("proj")).unwrap();
        std::fs::write(home.join("proj/in.txt"), b"in").unwrap();

        let at = |p: PathBuf| try_resolve_existing_under(&home, &p.to_string_lossy());

        assert!(at(home.join("proj/in.txt")).is_ok());
        // Gone, but inside the root: the leaf, and a whole missing subtree.
        assert_eq!(at(home.join("proj/gone.md")), Err(Resolved::Missing));
        assert_eq!(at(home.join("proj/no/such/dir/x")), Err(Resolved::Missing));
        assert_eq!(at(home.join("gone-dir")), Err(Resolved::Missing));
        // Outside the root, existing or not — one answer.
        assert_eq!(try_resolve_existing_under(&home, "/etc/passwd"), Err(Resolved::Outside));
        assert_eq!(try_resolve_existing_under(&home, "/etc/nope-not-here"), Err(Resolved::Outside));
        assert_eq!(try_resolve_existing_under(&home, "relative"), Err(Resolved::Outside));
        assert_eq!(at(base.join("sibling")), Err(Resolved::Outside));

        let _ = std::fs::remove_dir_all(&base);
    }

    /// The oracle test. A symlink under the root pointing OUT of it must answer
    /// identically whether or not the target exists — otherwise the 404/403 split
    /// is a per-path existence probe for the entire filesystem. See [`Resolved`].
    #[cfg(unix)]
    #[test]
    fn symlink_outward_never_discloses() {
        use std::os::unix::fs::symlink;
        let base = std::env::temp_dir().join(format!("ccr-confine-oracle-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let home = base.join("home");
        let outside = base.join("outside");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("secret.txt"), b"x").unwrap();
        symlink(&outside, home.join("escape")).unwrap();

        let existing = try_resolve_existing_under(&home, &home.join("escape/secret.txt").to_string_lossy());
        let absent = try_resolve_existing_under(&home, &home.join("escape/no-such-file").to_string_lossy());
        assert_eq!(existing, Err(Resolved::Outside));
        assert_eq!(absent, Err(Resolved::Outside));
        assert_eq!(existing, absent, "existence outside the root must not be observable");

        // A dangling symlink INSIDE the root is missing, not outside.
        symlink(home.join("nowhere"), home.join("dangling")).unwrap();
        assert_eq!(
            try_resolve_existing_under(&home, &home.join("dangling").to_string_lossy()),
            Err(Resolved::Missing)
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn safe_rel_rules() {
        assert_eq!(safe_rel("src/icons/a.svg"), Some(PathBuf::from("src/icons/a.svg")));
        assert!(safe_rel("../etc/x").is_none());
        assert!(safe_rel("/abs/x").is_none());
        assert!(safe_rel("a\\b").is_none());
        assert!(safe_rel("   ").is_none());
    }
}

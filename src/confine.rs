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

/// Resolve a path that must already EXIST and stay within `root` **even after
/// symlink resolution**. Lexical clean + lexical containment first (a cheap
/// reject of obvious traversal / non-absolute input), then canonicalize the
/// target and assert real-path containment — so a symlink under the root pointing
/// outside it is rejected, while a symlink whose target stays inside is allowed.
///
/// Returns the *lexical* path (for display + the subsequent fs op); the op
/// re-follows the symlink to the target we just verified is inside the root.
pub fn resolve_existing_under(root: &Path, p: &str) -> Option<PathBuf> {
    let lexical = resolve_under(root, p)?;
    let real_root = std::fs::canonicalize(root).ok()?;
    let real = std::fs::canonicalize(&lexical).ok()?;
    contained(&real, &real_root).then_some(lexical)
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

    #[test]
    fn safe_rel_rules() {
        assert_eq!(safe_rel("src/icons/a.svg"), Some(PathBuf::from("src/icons/a.svg")));
        assert!(safe_rel("../etc/x").is_none());
        assert!(safe_rel("/abs/x").is_none());
        assert!(safe_rel("a\\b").is_none());
        assert!(safe_rel("   ").is_none());
    }
}

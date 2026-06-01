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

    #[test]
    fn safe_rel_rules() {
        assert_eq!(safe_rel("src/icons/a.svg"), Some(PathBuf::from("src/icons/a.svg")));
        assert!(safe_rel("../etc/x").is_none());
        assert!(safe_rel("/abs/x").is_none());
        assert!(safe_rel("a\\b").is_none());
        assert!(safe_rel("   ").is_none());
    }
}

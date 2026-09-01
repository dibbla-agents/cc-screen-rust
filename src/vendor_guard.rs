// Snapshot/restore around Grok's official installers (proposal 0089).
//
// The vendor curl/PS installers always mutate shell rc or the Windows User PATH
// and have no skip flag. cc-screen's built-in path must not leave that mutation
// in place: snapshot the files/values the installer is documented to touch, run
// the command, restore them, then expose `~/.grok/bin/grok` through `~/.local/bin`.
// An explicit `cc_tool_install` / `cc_tool_update` skips this wrapper.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::assistants::{self, Run};
use crate::tools::Tool;

/// Env vars the official Grok installer honors that a built-in vendor command
/// must not inherit. `GROK_BIN_DIR` relocates the binary off `~/.grok/bin`;
/// `GROK_DEPLOYMENT_KEY` / `XAI_API_KEY` must never ride a dashboard job.
pub const STRIP_ENV: &[&str] = &[
    "GROK_BIN_DIR",
    "GROK_HOME",
    "GROK_CHANNEL",
    "GROK_DEPLOYMENT_KEY",
    "GROK_PROXY_URL",
    "XAI_API_KEY",
];

pub fn wraps_install(t: &Tool) -> bool {
    t.prefix == "grok" && t.install_hint.is_none()
}

pub fn wraps_update(t: &Tool) -> bool {
    t.prefix == "grok" && t.update_cmd.is_none()
}

/// Planned restore of a Windows User PATH string. Pure so Linux CI can inject
/// before/after values ([0051] "platform behavior as inputs").
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(not(windows), allow(dead_code))]
pub enum UserPathRestore {
    Unchanged,
    Write(String),
    Delete,
}

#[cfg_attr(not(windows), allow(dead_code))]
pub fn plan_user_path_restore(before: Option<&str>, after: Option<&str>) -> UserPathRestore {
    match (before, after) {
        (a, b) if a == b => UserPathRestore::Unchanged,
        (Some(b), _) => UserPathRestore::Write(b.to_string()),
        (None, Some(_)) => UserPathRestore::Delete,
        (None, None) => UserPathRestore::Unchanged,
    }
}

enum RcKind {
    Missing,
    Regular { bytes: Vec<u8> },
    Symlink { target: PathBuf, bytes: Vec<u8> },
}

struct RcSnap {
    path: PathBuf,
    kind: RcKind,
}

pub struct Guard {
    home: PathBuf,
    rcs: Vec<RcSnap>,
    bak_before: HashSet<PathBuf>,
    extra_bins: Vec<(PathBuf, bool)>,
    agent_landing: PathBuf,
    agent_landing_existed: bool,
    user_path_before: Option<String>,
    finished: bool,
}

impl Guard {
    pub fn begin(home: &Path) -> Result<Self, String> {
        Self::begin_with(home, &PathBuf::from("/usr/local/bin"))
    }

    pub fn begin_with(home: &Path, extra_bin_dir: &Path) -> Result<Self, String> {
        let rcs = snapshot_rcs(home)?;
        let mut bak_before = HashSet::new();
        for rc in &rcs {
            for p in bak_siblings(&rc.path) {
                bak_before.insert(p);
            }
        }
        let extra_names = ["grok", "agent", "grok.exe", "agent.exe"];
        let extra_bins = extra_names
            .iter()
            .map(|n| {
                let p = extra_bin_dir.join(n);
                let existed = std::fs::symlink_metadata(&p).is_ok();
                (p, existed)
            })
            .collect();
        let agent_landing = home.join(".local").join("bin").join(if cfg!(windows) { "agent.exe" } else { "agent" });
        let agent_landing_existed = std::fs::symlink_metadata(&agent_landing).is_ok();
        Ok(Guard {
            home: home.to_path_buf(),
            rcs,
            bak_before,
            extra_bins,
            agent_landing,
            agent_landing_existed,
            user_path_before: read_user_path(),
            finished: false,
        })
    }

    pub fn run(
        &self,
        line: &str,
        env_path: &str,
        timeout: std::time::Duration,
        extra_env: &[(&str, String)],
    ) -> Run {
        let home = self.home.to_string_lossy().into_owned();
        let mut vars: Vec<(&str, String)> = extra_env.to_vec();
        vars.retain(|(k, _)| *k != "HOME" && *k != "USERPROFILE");
        vars.push(("HOME", home.clone()));
        if cfg!(windows) {
            vars.push(("USERPROFILE", home));
        }
        assistants::run_shell_ex(line, env_path, timeout, &vars, STRIP_ENV)
    }

    pub fn finish(mut self) -> Result<(), String> {
        let r = self.restore();
        self.finished = true;
        r
    }

    fn restore(&mut self) -> Result<(), String> {
        let mut err: Option<String> = None;
        for rc in &self.rcs {
            if let Err(e) = restore_rc(&self.home, rc) {
                err = Some(e);
            }
        }
        for rc in &self.rcs {
            for p in bak_siblings(&rc.path) {
                if !self.bak_before.contains(&p) {
                    let _ = std::fs::remove_file(&p);
                }
            }
        }
        if !self.agent_landing_existed && std::fs::symlink_metadata(&self.agent_landing).is_ok() {
            if let Err(e) = std::fs::remove_file(&self.agent_landing) {
                err = Some(format!("could not remove {}: {e}", self.agent_landing.display()));
            }
        }
        for (p, existed) in &self.extra_bins {
            if !existed && std::fs::symlink_metadata(p).is_ok() {
                if std::fs::remove_file(p).is_err() {
                    err = Some(format!(
                        "installer wrote {} (outside $HOME) and it could not be removed",
                        p.display()
                    ));
                }
            }
        }
        if let Err(e) = restore_user_path(self.user_path_before.as_deref()) {
            err = Some(e);
        }
        match err {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        if !self.finished {
            let _ = self.restore();
        }
    }
}

fn rc_paths(home: &Path) -> Vec<PathBuf> {
    vec![
        home.join(".bashrc"),
        home.join(".zshrc"),
        home.join(".bash_profile"),
        home.join(".config").join("fish").join("config.fish"),
    ]
}

fn snapshot_rcs(home: &Path) -> Result<Vec<RcSnap>, String> {
    let mut out = Vec::new();
    for path in rc_paths(home) {
        out.push(snapshot_one(home, path)?);
    }
    Ok(out)
}

fn snapshot_one(home: &Path, path: PathBuf) -> Result<RcSnap, String> {
    let md = match std::fs::symlink_metadata(&path) {
        Ok(m) => m,
        Err(_) => {
            return Ok(RcSnap { path, kind: RcKind::Missing });
        }
    };
    if md.file_type().is_symlink() {
        let target = std::fs::read_link(&path).map_err(|e| format!("readlink {}: {e}", path.display()))?;
        let resolved = if target.is_absolute() {
            target.clone()
        } else {
            path.parent().unwrap_or(Path::new(".")).join(&target)
        };
        if !resolved.starts_with(home) {
            return Err(format!(
                "{} is a symlink to {} (outside $HOME) — refusing to follow it",
                path.display(),
                resolved.display()
            ));
        }
        let bytes = std::fs::read(&resolved).unwrap_or_default();
        return Ok(RcSnap { path, kind: RcKind::Symlink { target, bytes } });
    }
    let bytes = std::fs::read(&path).map_err(|e| format!("read {}: {e}", path.display()))?;
    Ok(RcSnap { path, kind: RcKind::Regular { bytes } })
}

fn restore_rc(home: &Path, snap: &RcSnap) -> Result<(), String> {
    match &snap.kind {
        RcKind::Missing => {
            if std::fs::symlink_metadata(&snap.path).is_ok() {
                std::fs::remove_file(&snap.path)
                    .map_err(|e| format!("could not unlink installer-created {}: {e}", snap.path.display()))?;
            }
            Ok(())
        }
        RcKind::Regular { bytes } => {
            let md = std::fs::symlink_metadata(&snap.path).ok();
            if md.as_ref().is_some_and(|m| m.file_type().is_symlink()) {
                std::fs::remove_file(&snap.path)
                    .map_err(|e| format!("could not replace symlink {}: {e}", snap.path.display()))?;
            }
            if let Some(parent) = snap.path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            std::fs::write(&snap.path, bytes)
                .map_err(|e| format!("could not restore {}: {e}", snap.path.display()))
        }
        RcKind::Symlink { target, bytes } => {
            let resolved = if target.is_absolute() {
                target.clone()
            } else {
                snap.path.parent().unwrap_or(Path::new(".")).join(target)
            };
            if !resolved.starts_with(home) {
                return Err(format!(
                    "{} points outside $HOME ({}) — not restoring",
                    snap.path.display(),
                    resolved.display()
                ));
            }
            let current_link = std::fs::read_link(&snap.path).ok();
            if current_link.as_ref() != Some(target) {
                let _ = std::fs::remove_file(&snap.path);
                #[cfg(unix)]
                {
                    std::os::unix::fs::symlink(target, &snap.path).map_err(|e| {
                        format!("could not restore symlink {}: {e}", snap.path.display())
                    })?;
                }
                #[cfg(not(unix))]
                {
                    std::fs::write(&snap.path, bytes).map_err(|e| {
                        format!("could not restore {}: {e}", snap.path.display())
                    })?;
                    return Ok(());
                }
            }
            std::fs::write(&resolved, bytes)
                .map_err(|e| format!("could not restore {}: {e}", resolved.display()))
        }
    }
}

fn bak_siblings(path: &Path) -> Vec<PathBuf> {
    let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
        return Vec::new();
    };
    let Some(parent) = path.parent() else {
        return Vec::new();
    };
    let prefix = format!("{name}.bak.");
    let Ok(rd) = std::fs::read_dir(parent) else {
        return Vec::new();
    };
    rd.flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|s| s.to_str())
                .map(|s| {
                    s.starts_with(&prefix) && s[prefix.len()..].chars().all(|c| c.is_ascii_digit())
                })
                .unwrap_or(false)
        })
        .collect()
}

#[cfg(windows)]
fn read_user_path() -> Option<String> {
    read_user_path_windows()
}

#[cfg(not(windows))]
fn read_user_path() -> Option<String> {
    None
}

#[cfg(windows)]
fn restore_user_path(before: Option<&str>) -> Result<(), String> {
    let after = read_user_path_windows();
    match plan_user_path_restore(before, after.as_deref()) {
        UserPathRestore::Unchanged => Ok(()),
        UserPathRestore::Write(v) => write_user_path_windows(Some(&v)),
        UserPathRestore::Delete => write_user_path_windows(None),
    }
}

#[cfg(not(windows))]
fn restore_user_path(_before: Option<&str>) -> Result<(), String> {
    Ok(())
}

#[cfg(windows)]
fn read_user_path_windows() -> Option<String> {
    // Same user-scope API the official installer uses. In-memory only — never
    // a %TEMP% snapshot file (rc/PATH values can hold secrets).
    let output = std::process::Command::new("powershell")
        .args([
            "-NoProfile",
            "-Command",
            "[Environment]::GetEnvironmentVariable('Path','User')",
        ])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

#[cfg(windows)]
fn write_user_path_windows(value: Option<&str>) -> Result<(), String> {
    let arg = match value {
        Some(v) => format!(
            "[Environment]::SetEnvironmentVariable('Path', @'\n{v}\n'@, 'User')"
        ),
        None => "[Environment]::SetEnvironmentVariable('Path', $null, 'User')".into(),
    };
    let status = std::process::Command::new("powershell")
        .args(["-NoProfile", "-Command", &arg])
        .status()
        .map_err(|e| format!("could not restore User PATH: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err("could not restore User PATH".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_path_restore_is_pure_over_injected_values() {
        assert_eq!(
            plan_user_path_restore(Some("a;b"), Some("C:\\Users\\x\\.grok\\bin;a;b")),
            UserPathRestore::Write("a;b".into())
        );
        assert_eq!(
            plan_user_path_restore(None, Some("C:\\Users\\x\\.grok\\bin")),
            UserPathRestore::Delete
        );
        assert_eq!(
            plan_user_path_restore(Some("a"), Some("a")),
            UserPathRestore::Unchanged
        );
        assert_eq!(plan_user_path_restore(None, None), UserPathRestore::Unchanged);
    }

    #[test]
    fn wrap_skips_operator_overrides() {
        let mut t = Tool::new("gk", "grok", "grok");
        assert!(wraps_install(&t));
        assert!(wraps_update(&t));
        t.install_hint = Some("my-mirror install grok".into());
        assert!(!wraps_install(&t));
        t.install_hint = None;
        t.update_cmd = Some("my-mirror update grok".into());
        assert!(!wraps_update(&t));
        let other = Tool::new("oc", "opencode", "opencode");
        assert!(!wraps_install(&other));
        assert!(!wraps_update(&other));
    }

    #[test]
    fn strip_env_covers_the_relocators_and_secrets() {
        for k in [
            "GROK_BIN_DIR",
            "GROK_HOME",
            "GROK_CHANNEL",
            "GROK_DEPLOYMENT_KEY",
            "GROK_PROXY_URL",
            "XAI_API_KEY",
        ] {
            assert!(STRIP_ENV.contains(&k), "{k}");
        }
    }
}

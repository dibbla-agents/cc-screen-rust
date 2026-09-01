// The tool registry — a direct port of the Go build's tmux.go parsing, minus
// the tmux specifics. `cc_tool <cmd> <prefix> <launch-template>` is the single
// source of truth shared with the shell tool; {name} in the template is the
// session's short name. We additionally know each tool's extra-dir startup flag
// and its resume form (for the restore path), with built-in defaults for the
// four known CLIs.

use std::path::{Path, PathBuf};

use cc_screen_protocol::{ExtraDirs, ToolInfo};

/// How a staged clipboard image reaches this tool's PTY (proposal 0066).
/// Server-internal — never a wire field, never chosen by a client.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ImagePasteStrategy {
    /// Send Ctrl-V (0x16); the assistant shells out to xclip/wl-paste/pbpaste
    /// and the cc-screen shim serves the staged PNG (proposal 0007). Claude
    /// Code's contract, and the backward-compatible default for every tool.
    ClipboardProbe,
    /// Stage the PNG as a private local file and bracketed-paste its
    /// shell-escaped absolute path — no Enter, no Ctrl-V. Codex reads the
    /// clipboard natively via X11/Wayland (absent on a headless box), but it
    /// recognises an explicitly pasted readable image path and attaches it.
    BracketedImagePath,
}

#[derive(Clone)]
pub struct Tool {
    pub cmd: String,    // shell command, e.g. cc
    pub prefix: String, // session-name prefix, e.g. claude
    pub tmpl: String,   // launch template; {name} -> session short name
    pub extra_flag: Option<String>, // --add-dir / --include-directories
    pub extra_max: u32,             // 0 = unlimited
    pub resume_suffix: Option<String>, // e.g. "--continue", "resume --last"
    pub resume_keep_extra: bool,
    /// The approval-bypass flag, kept *separate* from `tmpl` so a session can
    /// launch without it (proposal 0005's per-session "skip permissions"
    /// switch). `None` = the tool has no such flag (the bare shell) **or** a
    /// pre-0005 user template that still inlines it — either way nothing is
    /// appended, and a `false` skip can't strip an inlined flag.
    pub yolo_flag: Option<String>,
    /// A user-declared install one-liner (`cc_tool_install`, proposal 0046) shown
    /// when the tool's binary is missing. `None` falls back to the built-in
    /// assistant registry (`install_hint`), or to a plain "not installed" message
    /// for a custom tool with no hint — the guard still detects the miss.
    pub install_hint: Option<String>,
    /// A user-declared update one-liner (`cc_tool_update`, proposal 0049). Wins
    /// over the assistant registry's self-update / package-manager commands — the
    /// escape hatch for a pinned version, a wrapper, or a distro package. `None`
    /// falls back to the registry (`update_commands`).
    pub update_cmd: Option<String>,
    /// How a pasted clipboard image is delivered to this tool's PTY (0066).
    /// Filled by `with_defaults` from the same `(prefix, cmd)` identity the
    /// other built-in metadata uses, so a `tools.conf` Codex override inherits
    /// it; a genuinely unknown custom tool keeps the compatible probe.
    pub image_paste: ImagePasteStrategy,
    /// The flag appended when the session's assistant-remote-control switch is
    /// **off** — the default (proposal 0082). For Claude Code this is
    /// `--settings <disable-file>`, which is the only reliable off-switch: it
    /// beats the user's own `remoteControlAtStartup`, the in-session
    /// `/remote-control`, resume re-registration and any upstream auto-on
    /// cohort, for this invocation only. `None` = **stance-free**: nothing is
    /// appended and the user's own settings govern (the tool has no such
    /// feature, an operator cleared it with `cc_tool_remote_off <tool> ""`, or
    /// the template already carries the feature by hand — the inline guard).
    pub remote_off_flag: Option<String>,
    /// The flag appended when the switch is **on**: for Claude Code
    /// `--rc claude-{name}`, [0015]'s documented opt-in under a cc-screen-derived
    /// name (`{name}` is substituted in `build_launch`, like the template's).
    /// `None` = there is nothing to turn on, so no create surface offers the
    /// switch and an opted-in session relaunches stance-free.
    pub remote_on_flag: Option<String>,
}

impl Tool {
    pub(crate) fn new(cmd: &str, prefix: &str, tmpl: &str) -> Tool {
        Tool {
            cmd: cmd.into(),
            prefix: prefix.into(),
            tmpl: tmpl.into(),
            extra_flag: None,
            extra_max: 0,
            resume_suffix: None,
            resume_keep_extra: false,
            yolo_flag: None,
            install_hint: None,
            update_cmd: None,
            image_paste: ImagePasteStrategy::ClipboardProbe,
            remote_off_flag: None,
            remote_on_flag: None,
        }
    }
}

/// The wire DTO for a tool. Shared by `GET /api/tools` and the hub uplink's
/// `Register` so both advertise the registry identically. `unavailable` is the
/// live probe result (proposal 0046): true when the tool's CLI isn't on the
/// session PATH, so a picker can grey it out before a doomed create.
pub fn tool_info(t: &Tool, unavailable: bool) -> ToolInfo {
    ToolInfo {
        cmd: t.cmd.clone(),
        prefix: t.prefix.clone(),
        extra_dirs: t
            .extra_flag
            .is_some()
            .then(|| ExtraDirs { max: (t.extra_max > 0).then_some(t.extra_max) }),
        unavailable,
        // 0082: only a tool we can actually *enable* remote control for gets the
        // per-session switch offered — a switch that changes nothing is worse
        // than no switch.
        remote_control_available: t.remote_on_flag.is_some(),
    }
}

// ── The supported-assistants registry (proposal 0046) ────────────────────────
// The single source of truth for "which coding assistants, and how to install
// them". Consumed by the runtime guard (`create_core`'s 424), `restore_all`'s
// skip, `GET /api/tools`' `unavailable`, and the `doctor` subcommand the shell
// installers call — so the installer and the runtime can never disagree.
// Adding an assistant is one entry here (plus its `defaults()` tool line).

pub struct Assistant {
    /// The tool registry key this describes (`Tool.prefix`).
    pub prefix: &'static str,
    /// Human name for messages ("Claude Code").
    pub label: &'static str,
    /// Install one-liner per platform — each CLI's official installer. Kept
    /// data-only so a stale vendor command is a one-line fix here. A Windows
    /// entry that needs PowerShell carries its own `powershell -NoProfile …`
    /// wrapper, because the launch shell there is `cmd.exe /C` (proposal 0050
    /// Part H) — the runner stays platform-agnostic.
    pub install_macos: &'static str,
    pub install_linux: &'static str,
    pub install_windows: &'static str,
    /// Fallback URL for platforms with no one-liner (or for humans who want it).
    pub docs: &'static str,
    /// Prerequisite keys (see `PREREQS`) this assistant's install command needs
    /// on each platform. OpenCode is self-contained on Unix but needs npm on
    /// native Windows (proposal 0088).
    pub needs_macos: &'static [&'static str],
    pub needs_linux: &'static [&'static str],
    pub needs_windows: &'static [&'static str],
    /// Rough download footprint, shown in the confirmation dialog so a metered
    /// connection isn't surprised. Registry data, never measured at runtime; the
    /// Windows numbers are genuinely different (verified: `@openai/codex` is
    /// 407 MB there), so they're their own column.
    pub size_hint: &'static str,
    pub size_hint_windows: &'static str,
    /// The CLI's own self-update subcommand, if it has one — the FIRST candidate,
    /// because the CLI knows how it was installed. `""` = it has none, so the
    /// package-manager command below is the only path (proposal 0049).
    pub self_update: &'static str,
    /// The package-manager route to the latest version, per platform. Used when
    /// there is no self-update subcommand, AND as the fallback when one ran but
    /// the version didn't move (a self-updater on a package-manager-installed
    /// copy legitimately refuses).
    pub update_macos: &'static str,
    pub update_linux: &'static str,
    pub update_windows: &'static str,
    /// The argument that prints the version. `--version` for the built-ins
    /// (verified), but kept per-descriptor so a new assistant that spells it
    /// differently needs no code change.
    pub version_arg: &'static str,
}

/// A prerequisite: a *tool-chain* dependency of an assistant's install command,
/// not an assistant itself (proposal 0050 Part A). Same data-only shape and the
/// same "installed for the local user, never `sudo`" rule.
pub struct Prereq {
    /// The key an `Assistant.needs` entry names.
    pub key: &'static str,
    /// The binary probed on the session PATH to decide "is it already here?".
    pub bin: &'static str,
    /// Human name for the dialog ("Node.js (npm)").
    pub label: &'static str,
    pub install_macos: &'static str,
    pub install_linux: &'static str,
    /// Empty = no user-scope bootstrap on this platform; we report and link to
    /// the docs rather than guessing (Node on Windows is a machine-scope MSI).
    pub install_windows: &'static str,
    pub docs: &'static str,
    /// Extra binaries to expose in the landing zone after installing: a Node
    /// tarball yields node/npm/npx under its own prefix.
    pub also_link: &'static [&'static str],
    pub size_hint: &'static str,
    pub size_hint_windows: &'static str,
}

/// Unpack the official Node distribution into `$HOME/.local/share/node` — which
/// is exactly what a working box looks like, and a writable `npm` prefix by
/// construction, so `npm install -g` needs no root. The version is data
/// (`CCWEB_NODE_VERSION`); platform/arch come from `uname`.
const NODE_TARBALL_INSTALL: &str = "set -e; \
V=\"${CCWEB_NODE_VERSION:-v22.20.0}\"; \
P=$(uname -s | tr 'A-Z' 'a-z'); \
A=$(uname -m); case \"$A\" in x86_64|amd64) A=x64;; aarch64|arm64) A=arm64;; esac; \
D=\"$HOME/.local/share/node\"; mkdir -p \"$D\"; \
curl -fsSL \"https://nodejs.org/dist/$V/node-$V-$P-$A.tar.xz\" | tar -xJ -C \"$D\" --strip-components=1";

pub const PREREQS: &[Prereq] = &[
    Prereq {
        key: "npm",
        bin: "npm",
        label: "Node.js (npm)",
        install_macos: NODE_TARBALL_INSTALL,
        install_linux: NODE_TARBALL_INSTALL,
        // Node's Windows distribution is an MSI / machine-scope install, so an
        // absent `npm` is REPORTED with the docs link, never guessed at (0050
        // Non-Goals). Verified: `harebell` has it at C:\Program Files\nodejs.
        install_windows: "",
        docs: "https://nodejs.org/en/download",
        also_link: &["node", "npm", "npx"],
        size_hint: "~30 MB (≈120 MB unpacked)",
        size_hint_windows: "",
    },
    Prereq {
        key: "uv",
        bin: "uv",
        label: "uv",
        // Astral's own user-local installer — lands `uv` in ~/.local/bin
        // (verified). `kimi` then brings its own Python via `uv tool install`,
        // so Python is not a separate prerequisite.
        install_macos: "curl -LsSf https://astral.sh/uv/install.sh | sh",
        install_linux: "curl -LsSf https://astral.sh/uv/install.sh | sh",
        // `UV_NO_MODIFY_PATH=1` / `UV_INSTALL_DIR` come from the install
        // environment (provision.rs), not from this string: an unattended
        // install must not rewrite the user's PATH behind their back.
        install_windows:
            "powershell -NoProfile -ExecutionPolicy Bypass -Command \"irm https://astral.sh/uv/install.ps1 | iex\"",
        docs: "https://docs.astral.sh/uv/getting-started/installation/",
        also_link: &["uv", "uvx"],
        size_hint: "~15 MB",
        size_hint_windows: "~74 MB",
    },
];

/// The prerequisite descriptor for a key, if it's one we know.
pub fn prereq(key: &str) -> Option<&'static Prereq> {
    PREREQS.iter().find(|p| p.key == key)
}

#[derive(Clone, Copy)]
enum AssistantPlatform { Macos, Linux, Windows }

fn assistant_platform() -> AssistantPlatform {
    if cfg!(target_os = "macos") { AssistantPlatform::Macos }
    else if cfg!(windows) { AssistantPlatform::Windows }
    else { AssistantPlatform::Linux }
}

fn prereqs_for_platform(t: &Tool, platform: AssistantPlatform) -> Vec<&'static Prereq> {
    if t.install_hint.is_some() {
        return Vec::new();
    }
    let Some(a) = assistant_for(t) else { return Vec::new() };
    let needs = match platform {
        AssistantPlatform::Macos => a.needs_macos,
        AssistantPlatform::Linux => a.needs_linux,
        AssistantPlatform::Windows => a.needs_windows,
    };
    needs.iter().filter_map(|k| prereq(k)).collect()
}

/// The prerequisites a tool's *built-in* install command needs on this host.
pub fn prereqs_for(t: &Tool) -> Vec<&'static Prereq> {
    prereqs_for_platform(t, assistant_platform())
}

/// This platform's install one-liner for a prerequisite, or `None` when there
/// is no user-scope bootstrap for it here (Windows Node).
pub fn prereq_install_command(p: &Prereq) -> Option<&'static str> {
    let cmd = if cfg!(target_os = "macos") {
        p.install_macos
    } else if cfg!(windows) {
        p.install_windows
    } else {
        p.install_linux
    };
    (!cmd.is_empty()).then_some(cmd)
}

/// This platform's rough download size for a prerequisite (empty = unknown).
pub fn prereq_size_hint(p: &Prereq) -> &'static str {
    if cfg!(windows) && !p.size_hint_windows.is_empty() { p.size_hint_windows } else { p.size_hint }
}

/// This platform's rough download size for a tool, when it's a known assistant.
pub fn size_hint(t: &Tool) -> &'static str {
    let Some(a) = assistant_for(t) else { return "" };
    if cfg!(windows) && !a.size_hint_windows.is_empty() { a.size_hint_windows } else { a.size_hint }
}

pub const ASSISTANTS: &[Assistant] = &[
    Assistant {
        prefix: "claude",
        label: "Claude Code",
        install_macos: "curl -fsSL https://claude.ai/install.sh | bash",
        install_linux: "curl -fsSL https://claude.ai/install.sh | bash",
        // Verified on `harebell`: the vendor's PowerShell installer drops a
        // single claude.exe in %USERPROFILE%\.local\bin, and `claude update`
        // works there (proposal 0050 Part H).
        install_windows:
            "powershell -NoProfile -ExecutionPolicy Bypass -Command \"irm https://claude.ai/install.ps1 | iex\"",
        docs: "https://docs.claude.com/en/docs/claude-code/setup",
        // Its installer is self-contained (curl + bash / irm + iex).
        needs_macos: &[], needs_linux: &[], needs_windows: &[],
        size_hint: "~60 MB",
        size_hint_windows: "~60 MB",
        self_update: "claude update",
        // The official installer is idempotent — re-running upgrades in place.
        update_macos: "curl -fsSL https://claude.ai/install.sh | bash",
        update_linux: "curl -fsSL https://claude.ai/install.sh | bash",
        update_windows:
            "powershell -NoProfile -ExecutionPolicy Bypass -Command \"irm https://claude.ai/install.ps1 | iex\"",
        version_arg: "--version",
    },
    Assistant {
        prefix: "codex",
        label: "Codex CLI",
        install_macos: "npm install -g @openai/codex",
        install_linux: "npm install -g @openai/codex",
        // Verified on `harebell`: exit 0 as a non-admin user; the default npm
        // prefix (%APPDATA%\npm) is user-writable AND already on the User PATH,
        // so no prefix override is needed there.
        install_windows: "npm install -g @openai/codex",
        docs: "https://github.com/openai/codex",
        needs_macos: &["npm"], needs_linux: &["npm"], needs_windows: &["npm"],
        size_hint: "~120 MB",
        size_hint_windows: "~407 MB", // measured on harebell (vendored binaries)
        self_update: "codex update",
        update_macos: "npm install -g @openai/codex@latest",
        update_linux: "npm install -g @openai/codex@latest",
        update_windows: "npm install -g @openai/codex@latest",
        version_arg: "--version",
    },
    Assistant {
        prefix: "gemini",
        label: "Gemini CLI",
        install_macos: "npm install -g @google/gemini-cli",
        install_linux: "npm install -g @google/gemini-cli",
        install_windows: "npm install -g @google/gemini-cli",
        docs: "https://github.com/google-gemini/gemini-cli",
        needs_macos: &["npm"], needs_linux: &["npm"], needs_windows: &["npm"],
        size_hint: "~30 MB",
        size_hint_windows: "~30 MB",
        // No self-update subcommand (verified) — npm is the only route.
        self_update: "",
        update_macos: "npm install -g @google/gemini-cli@latest",
        update_linux: "npm install -g @google/gemini-cli@latest",
        update_windows: "npm install -g @google/gemini-cli@latest",
        version_arg: "--version",
    },
    Assistant {
        prefix: "kimi",
        label: "Kimi CLI",
        install_macos: "uv tool install --python 3.13 kimi-cli",
        install_linux: "uv tool install --python 3.13 kimi-cli",
        // UNVERIFIED on Windows end-to-end (proposal 0050 Part H): the command is
        // documented cross-platform and `uv` itself bootstraps user-scope there,
        // but nobody has run this through to a launchable `kimi`. The failure
        // mode is an honest `failed` row carrying uv's own error. Whoever
        // verifies it on a Windows box should delete this comment.
        install_windows: "uv tool install --python 3.13 kimi-cli",
        docs: "https://github.com/MoonshotAI/kimi-cli",
        needs_macos: &["uv"], needs_linux: &["uv"], needs_windows: &["uv"],
        size_hint: "~15 MB (+ a CPython build)",
        size_hint_windows: "~15 MB (+ a CPython build)",
        // No self-update subcommand (verified) — uv owns the install.
        self_update: "",
        update_macos: "uv tool upgrade kimi-cli",
        update_linux: "uv tool upgrade kimi-cli",
        update_windows: "uv tool upgrade kimi-cli",
        version_arg: "--version",
    },
    Assistant {
        prefix: "opencode",
        label: "OpenCode",
        install_macos: "curl -fsSL https://opencode.ai/install | bash -s -- --no-modify-path",
        install_linux: "curl -fsSL https://opencode.ai/install | bash -s -- --no-modify-path",
        install_windows: "npm install -g opencode-ai",
        docs: "https://opencode.ai/docs/",
        needs_macos: &[], needs_linux: &[], needs_windows: &["npm"],
        size_hint: "~180 MB",
        size_hint_windows: "~540 MB",
        // The self-updater detects the active install method; no duplicate-copy fallback.
        self_update: "opencode upgrade",
        update_macos: "",
        update_linux: "",
        update_windows: "",
        version_arg: "--version",
    },
    Assistant {
        prefix: "grok",
        label: "Grok",
        install_macos: "curl -fsSL https://x.ai/cli/install.sh | bash",
        install_linux: "curl -fsSL https://x.ai/cli/install.sh | bash",
        // Official PS installer is self-contained (no Node). Wrapped for
        // `cmd.exe /C` the same way Claude's installer is (proposal 0089).
        install_windows:
            "powershell -NoProfile -ExecutionPolicy Bypass -Command \"irm https://x.ai/cli/install.ps1 | iex\"",
        docs: "https://docs.x.ai/build/overview",
        needs_macos: &[],
        needs_linux: &[],
        needs_windows: &[],
        // Measured 2026-09-01: official download ELF is 158.4 MB. Isolated
        // install is that plus the `~/.grok/bin` symlink; a lived-in home is
        // larger (`grok du`). Same download class on Windows until harebell.
        size_hint: "~160 MB",
        size_hint_windows: "~160 MB",
        // Detects installer = "internal" / npm / etc. No curl fallback: that
        // could plant a second copy while PATH still resolves the first.
        self_update: "grok update",
        update_macos: "",
        update_linux: "",
        update_windows: "",
        version_arg: "--version",
    },
];

/// The descriptor for a tool, if it's one of the known assistants.
pub fn assistant_for(t: &Tool) -> Option<&'static Assistant> {
    ASSISTANTS.iter().find(|a| a.prefix == t.prefix)
}

/// The binary a tool needs on PATH: the first bare token of its launch template
/// (`claude`, `codex`, …). `None` = nothing to probe — a head that's a shell
/// expansion (the `shell` tool's `${SHELL:-/bin/bash}`) always resolves. A
/// `tools.conf` override that renames the binary is probed correctly with no
/// extra config, because the template head IS what `/bin/sh -c` will exec.
pub fn probe_binary(t: &Tool) -> Option<&str> {
    let head = t.tmpl.split_whitespace().next()?;
    if head.starts_with('$') {
        return None;
    }
    Some(head)
}

/// The install one-liner to suggest when a tool's binary is missing, for the
/// *host* OS: an explicit `cc_tool_install` wins, else the built-in assistant
/// registry, else `None` (the guard still reports the miss, just without a
/// command). Data lives in one place; this only selects.
pub fn install_hint(t: &Tool) -> Option<String> {
    if let Some(cmd) = install_command(t) {
        return Some(cmd);
    }
    let a = assistant_for(t)?;
    Some(format!("see {}", a.docs))
}

/// The **runnable** install one-liner for this platform: a `cc_tool_install`
/// override, else the registry's per-OS column. `None` when this platform has
/// none — deliberately distinct from `install_hint`, which falls back to a
/// `"see <docs>"` *display* string that must never be handed to a shell
/// (proposal 0050 Part B2).
pub fn install_command(t: &Tool) -> Option<String> {
    if let Some(h) = &t.install_hint {
        return Some(h.clone());
    }
    let a = assistant_for(t)?;
    let cmd = if cfg!(target_os = "macos") {
        a.install_macos
    } else if cfg!(windows) {
        a.install_windows
    } else {
        a.install_linux
    };
    (!cmd.is_empty()).then(|| cmd.to_string())
}

/// The docs URL for a tool, when it's a known assistant.
pub fn docs_url(t: &Tool) -> Option<&'static str> {
    assistant_for(t).map(|a| a.docs)
}

/// The human label for a tool: the assistant descriptor's, else its prefix.
pub fn label(t: &Tool) -> String {
    assistant_for(t).map(|a| a.label.to_string()).unwrap_or_else(|| t.prefix.clone())
}

/// The ordered candidate commands that bring a tool up to date (proposal 0049),
/// most-authoritative first: an explicit `cc_tool_update` override, then the
/// CLI's own self-update subcommand (it knows how it was installed), then the
/// registry's package-manager command for this OS. Empty entries are dropped, so
/// a custom tool with neither an override nor a descriptor yields `[]` — reported
/// as "no known update command", never an error. Mirrors `install_hint`'s
/// precedence; the *list* is ordered because a self-updater on a
/// package-manager-installed copy may legitimately refuse to act, and the runner
/// decides by re-probing the version rather than by trusting an exit code.
pub fn update_commands(t: &Tool) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    if let Some(c) = t.update_cmd.as_ref().map(|s| s.trim()).filter(|s| !s.is_empty()) {
        out.push(c.to_string());
    }
    if let Some(a) = assistant_for(t) {
        for cand in [
            a.self_update,
            if cfg!(target_os = "macos") {
                a.update_macos
            } else if cfg!(windows) {
                a.update_windows
            } else {
                a.update_linux
            },
        ] {
            let cand = cand.trim();
            if !cand.is_empty() && !out.iter().any(|c| c == cand) {
                out.push(cand.to_string());
            }
        }
    }
    out
}

/// The argument that prints a tool's version (proposal 0049). The assistant
/// descriptor's when it's a known CLI, else `--version` — universal in practice,
/// and a wrong guess only yields an empty version string, which the runner
/// handles (it falls back to the command's exit code).
pub fn version_arg(t: &Tool) -> &'static str {
    assistant_for(t).map(|a| a.version_arg).unwrap_or("--version")
}

// ── Resolving a launch head on the session PATH (proposals 0046, 0051) ───────
// The probe must agree with the *launch*: if `launch_shell()`'s interpreter would
// find the binary, we say present; if it wouldn't, we say missing. On Unix that's
// `/bin/sh`'s lookup (':'-separated PATH, executable bit). On Windows it's
// `cmd.exe /C`'s (';'-separated PATH, `PATHEXT` candidate extensions, and the
// extension IS the executability signal) — 0051: splitting a Windows PATH on ':'
// matched nothing at all, which made every create/restore/update fail there.
//
// Both platforms run ONE implementation; the two differences are *inputs*
// (`Resolver`), so the Windows behaviour is unit-testable from a Linux box.

/// The PATH separator for this platform — must match `config::build_env_path`,
/// which builds the string we split (`src/config.rs`).
pub const PATH_SEP: char = if cfg!(windows) { ';' } else { ':' };

/// The documented `cmd.exe` fallback when `PATHEXT` is unset or empty.
const DEFAULT_PATHEXT: &str = ".COM;.EXE;.BAT;.CMD";

/// The platform-shaped inputs to a PATH lookup: how the PATH is split, which
/// extensions a bare name may gain, and what counts as executable. Constructed
/// from the host by `host()`; the explicit constructors exist so a test can
/// resolve a Windows-shaped PATH on Linux and vice versa.
pub struct Resolver {
    sep: char,
    /// Always starts with `""` (try the name as written first, so `claude.exe`
    /// on the PATH resolves as itself and never becomes `claude.exe.EXE`).
    exts: Vec<String>,
    /// Windows rules: `\` is also a separator, a drive prefix means "path", and
    /// `is_file()` — not an executable bit — is the test.
    windows: bool,
}

impl Resolver {
    /// The resolver for the host we're running on.
    pub fn host() -> Resolver {
        if cfg!(windows) {
            Resolver::windows(&std::env::var("PATHEXT").unwrap_or_default())
        } else {
            Resolver::unix()
        }
    }

    pub fn unix() -> Resolver {
        Resolver { sep: ':', exts: vec![String::new()], windows: false }
    }

    /// `pathext` is the raw `PATHEXT` value (`".COM;.EXE;…"`); empty falls back
    /// to what `cmd.exe` itself defaults to.
    pub fn windows(pathext: &str) -> Resolver {
        let mut exts = vec![String::new()];
        let raw = if pathext.trim().is_empty() { DEFAULT_PATHEXT } else { pathext };
        for e in raw.split(';').map(str::trim).filter(|e| !e.is_empty()) {
            let e = if e.starts_with('.') { e.to_string() } else { format!(".{e}") };
            if !exts.iter().any(|x| x.eq_ignore_ascii_case(&e)) {
                exts.push(e);
            }
        }
        Resolver { sep: ';', exts, windows: true }
    }

    /// Whether `bin` is already a path rather than a bare name — checked
    /// directly, mirroring the shell. Not `contains('/')`: `C:\tools\claude.exe`
    /// and `a\b` are paths too.
    fn is_path(&self, bin: &str) -> bool {
        if bin.contains('/') || std::path::Path::new(bin).is_absolute() {
            return true;
        }
        if self.windows {
            // A drive prefix (`C:…`) or a backslash component separator.
            let b = bin.as_bytes();
            return bin.contains('\\')
                || (b.get(1) == Some(&b':') && b.first().is_some_and(|c| c.is_ascii_alphabetic()));
        }
        false
    }

    /// Executable *for this platform*: the mode bit on Unix, the extension (i.e.
    /// merely being a file with a candidate suffix) on Windows.
    fn is_runnable(&self, p: &std::path::Path) -> bool {
        if self.windows {
            return p.is_file();
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            p.metadata().map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0).unwrap_or(false)
        }
        #[cfg(not(unix))]
        {
            p.is_file()
        }
    }

    /// The PATH's directory entries, in order — empty ones dropped and the
    /// surrounding quotes Windows allows around an entry stripped.
    fn entries<'a>(&self, env_path: &'a str) -> Vec<&'a str> {
        env_path
            .split(self.sep)
            .map(|d| d.trim().trim_matches('"'))
            .filter(|d| !d.is_empty())
            .collect()
    }

    /// Resolve `bin` against `env_path`, returning what was found.
    pub fn resolve(&self, bin: &str, env_path: &str) -> Option<PathBuf> {
        if bin.is_empty() {
            return None;
        }
        if self.is_path(bin) {
            let p = PathBuf::from(bin);
            // A path is still allowed to gain an extension (`C:\tools\claude`
            // → `claude.exe`), exactly as the interpreter would.
            return self.with_exts(&p);
        }
        for dir in self.entries(env_path) {
            if let Some(hit) = self.with_exts(&std::path::Path::new(dir).join(bin)) {
                return Some(hit);
            }
        }
        None
    }

    /// `base` as written first, then each candidate extension appended.
    fn with_exts(&self, base: &std::path::Path) -> Option<PathBuf> {
        for ext in &self.exts {
            let cand = if ext.is_empty() {
                base.to_path_buf()
            } else {
                let mut s = base.as_os_str().to_os_string();
                s.push(ext);
                PathBuf::from(s)
            };
            if self.is_runnable(&cand) {
                return Some(cand);
            }
        }
        None
    }
}

/// Whether `bin` resolves to something launchable on `env_path` — the same
/// lookup this platform's launch interpreter performs (`launch_shell`), so the
/// guard's verdict and the spawn agree.
pub fn binary_on_path(bin: &str, env_path: &str) -> bool {
    resolve_on_path(bin, env_path).is_some()
}

/// The same search, returning **what it found** — the landing-zone step
/// (proposal 0050) needs the file, and `doctor` shows it in the ✓ row.
pub fn resolve_on_path(bin: &str, env_path: &str) -> Option<PathBuf> {
    Resolver::host().resolve(bin, env_path)
}

/// `Some(binary)` iff the tool needs a binary that is NOT on `env_path` —
/// the one predicate behind the create-time 424, the restore skip, and the
/// `unavailable` DTO field. `None` = fine to launch (present, or nothing to
/// probe, like the shell tool).
pub fn missing_binary(t: &Tool, env_path: &str) -> Option<String> {
    let bin = probe_binary(t)?;
    if binary_on_path(bin, env_path) {
        None
    } else {
        Some(bin.to_string())
    }
}

/// Strip one layer of matching surrounding quotes, mirroring shell parsing.
fn unquote(s: &str) -> String {
    let s = s.trim();
    let b = s.as_bytes();
    if b.len() >= 2 {
        let q = b[0];
        if (q == b'"' || q == b'\'') && b[b.len() - 1] == q {
            return s[1..s.len() - 1].to_string();
        }
    }
    s.to_string()
}

/// Split a line into its leading whitespace-separated tokens, returning the
/// remainder (unsplit) after `n` tokens — so a launch template keeps its spaces.
fn split_head(line: &str, n: usize) -> (Vec<String>, String) {
    let mut toks = Vec::new();
    let mut rest = line.trim_start();
    for _ in 0..n {
        match rest.find(char::is_whitespace) {
            Some(i) => {
                toks.push(rest[..i].to_string());
                rest = rest[i..].trim_start();
            }
            None => {
                toks.push(rest.to_string());
                rest = "";
            }
        }
    }
    (toks, rest.to_string())
}

/// Load the tool registry. `config_dir` is the agent's own config directory —
/// where the remote-control disable settings file lives (proposal 0082), whose
/// absolute path is baked into `remote_off_flag` here so `build_launch` stays a
/// pure function over the tool.
pub fn load_tools(path: Option<PathBuf>, config_dir: &Path) -> Vec<Tool> {
    if let Some(p) = path {
        if let Ok(text) = std::fs::read_to_string(&p) {
            let parsed = parse(&text);
            if !parsed.is_empty() {
                return with_defaults(parsed, config_dir);
            }
        }
    }
    with_defaults(defaults(), config_dir)
}

// ── The assistant remote-control disable file (proposal 0082) ────────────────
// Claude Code has no `--no-remote-control` flag and no disable env var; the only
// per-invocation off-switch is `--settings <file|json>`, whose `flagSettings`
// source outranks the user's own settings for `disableRemoteControl`. We pass a
// FILE rather than inline JSON: inline JSON would need different quoting under
// `/bin/sh -c` and `cmd.exe /C` — exactly the class of Windows breakage 0051
// cleaned up — while a path rides the platform-split `shell_quote` unchanged.

/// The disable file's name inside the agent's config dir.
pub const REMOTE_OFF_FILE: &str = "claude-remote-off.json";
/// Its entire content. Exactly one key: the semantic kill switch upstream
/// documents for claude.ai/code registration, the `claude remote-control`
/// daemon, `--remote-control`/`--rc`, auto-start, and the in-session toggle.
pub const REMOTE_OFF_JSON: &str = r#"{"disableRemoteControl": true}"#;

pub fn remote_off_path(config_dir: &Path) -> PathBuf {
    config_dir.join(REMOTE_OFF_FILE)
}

/// Write-or-verify the disable file. Called at boot **and on every launch path**,
/// so a wiped config dir can never leave a session launching with `--settings` at
/// a dangling path. Returns whether the file is now present with the right
/// content (best-effort: the caller logs, it never blocks a create).
pub fn ensure_remote_off_file(config_dir: &Path) -> bool {
    let path = remote_off_path(config_dir);
    if matches!(std::fs::read_to_string(&path), Ok(s) if s.trim() == REMOTE_OFF_JSON) {
        return true;
    }
    let _ = std::fs::create_dir_all(config_dir);
    std::fs::write(&path, REMOTE_OFF_JSON).is_ok()
}

/// Does this launch template already carry the remote-control feature by hand?
/// Then cc-screen appends no stance at all: we never strip, double, or
/// contradict an operator-written flag (the same discipline `yolo_flag` uses).
///
/// `--remote-control-session-name-prefix` deliberately does **not** trip this —
/// it is naming-only, and a false positive here would silently leave that
/// operator's sessions remote-control-capable.
fn inlines_remote_control(tmpl: &str) -> bool {
    // The disable key only ever appears *inside* a `--settings` argument, so
    // token equality can never see it — substring is the right test.
    if tmpl.contains("disableRemoteControl") {
        return true;
    }
    tmpl.split_whitespace().any(|tok| {
        tok == "--rc"
            || tok == "--remote-control"
            || tok.starts_with("--rc=")
            || tok.starts_with("--remote-control=")
    })
}

fn defaults() -> Vec<Tool> {
    // The approval-bypass flag is *not* baked into these templates — it lives in
    // `yolo_flag` (filled by `with_defaults`) so a session can opt out of YOLO
    // per 0005. Gemini's `--skip-trust` stays: it's a trust-store convenience,
    // not the dangerous approval bypass.
    vec![
        // Plain `claude` by default (0015): the `--rc`/`--remote-control` flag
        // registered every session with the Claude *desktop app* ("Remote control
        // active"), a surprising outward-facing side effect cc-screen never needs
        // (it drives sessions over its own agent/hub/PTY path; resume uses
        // `--continue`, not the registered name). Opt back in via tools.conf:
        //   cc_tool cc claude "claude --rc 'claude-{name}'"
        Tool::new("cc", "claude", "claude"),
        Tool::new("kc", "kimi", "kimi"),
        Tool::new("gc", "gemini", "gemini --skip-trust"),
        Tool::new("coc", "codex", "codex"),
        Tool::new("oc", "opencode", "opencode"),
        Tool::new("gk", "grok", "grok"),
        Tool::new("tt", "shell", SHELL_TMPL),
    ]
}

/// The bare-shell tool's launch template. POSIX default; on Windows there's no
/// `$SHELL`/`-l`, so fall back to PowerShell (present on every Win10+/11 box).
#[cfg(windows)]
const SHELL_TMPL: &str = "powershell -NoLogo";
#[cfg(not(windows))]
const SHELL_TMPL: &str = "${SHELL:-/bin/bash} -l";

/// The interpreter that wraps a session's launch command line: `/bin/sh -c` on
/// Unix, `cmd.exe /C` on Windows (present everywhere, and it transparently runs
/// the `.cmd`/`.ps1` shims npm-installed CLIs like `claude`/`codex` ship as).
/// Returns `(program, prefix_args)`; the assembled launch string is appended as
/// the final argument. See `engine.rs`.
#[cfg(windows)]
pub fn launch_shell() -> (&'static str, &'static [&'static str]) {
    ("cmd.exe", &["/C"])
}
#[cfg(not(windows))]
pub fn launch_shell() -> (&'static str, &'static [&'static str]) {
    ("/bin/sh", &["-c"])
}

fn parse(text: &str) -> Vec<Tool> {
    let mut out: Vec<Tool> = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    let mut extra: Vec<(String, String, u32)> = Vec::new();
    let mut resumes: Vec<(String, String)> = Vec::new();
    let mut yolos: Vec<(String, String)> = Vec::new();
    let mut installs: Vec<(String, String)> = Vec::new();
    let mut updates: Vec<(String, String)> = Vec::new();
    let mut rc_offs: Vec<(String, String)> = Vec::new();
    let mut rc_ons: Vec<(String, String)> = Vec::new();

    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with("cc_tool_extra_dirs") {
            let (toks, _) = split_head(line, 4);
            if toks.len() >= 3 {
                let max = toks.get(3).and_then(|s| s.parse::<u32>().ok()).unwrap_or(0);
                extra.push((toks[1].clone(), toks[2].clone(), max));
            }
            continue;
        }
        // cc_tool_yolo <cmd|prefix> <flag> — declare a tool's approval-bypass
        // flag separately so a session can launch without it (0005). The rest of
        // the line (after the key) is the flag, so a multi-token flag works.
        if line.starts_with("cc_tool_yolo") {
            let (toks, rest) = split_head(line, 2);
            if toks.len() >= 2 && !rest.is_empty() {
                yolos.push((toks[1].clone(), unquote(&rest)));
            }
            continue;
        }
        // cc_tool_install <cmd|prefix> "<install command>" — a custom tool's
        // install hint, shown when its binary is missing (0046). Same grammar as
        // the other cc_tool_* metadata lines; the rest of the line is the command.
        if line.starts_with("cc_tool_install") {
            let (toks, rest) = split_head(line, 2);
            if toks.len() >= 2 && !rest.is_empty() {
                installs.push((toks[1].clone(), unquote(&rest)));
            }
            continue;
        }
        // cc_tool_update <cmd|prefix> "<update command>" — how to bring this tool
        // up to date (0049). Wins over the built-in registry's self-update /
        // package-manager commands: a pinned version, a wrapper, a distro package.
        if line.starts_with("cc_tool_update") {
            let (toks, rest) = split_head(line, 2);
            if toks.len() >= 2 && !rest.is_empty() {
                updates.push((toks[1].clone(), unquote(&rest)));
            }
            continue;
        }
        // cc_tool_remote_off / cc_tool_remote_on <cmd|prefix> "<flag>" — override
        // (or clear) this tool's assistant-remote-control stance flags (0082).
        // An explicitly empty flag ("") is the *stance-free* escape hatch: nothing
        // is appended and the user's own assistant settings govern. Declared here
        // as `Some("")`, normalised to `None` at the end of `with_defaults` — the
        // two-step is what lets "declared empty" beat the built-in default.
        if line.starts_with("cc_tool_remote_off") {
            let (toks, rest) = split_head(line, 2);
            if toks.len() >= 2 && !rest.is_empty() {
                rc_offs.push((toks[1].clone(), unquote(&rest)));
            }
            continue;
        }
        if line.starts_with("cc_tool_remote_on") {
            let (toks, rest) = split_head(line, 2);
            if toks.len() >= 2 && !rest.is_empty() {
                rc_ons.push((toks[1].clone(), unquote(&rest)));
            }
            continue;
        }
        if line.starts_with("cc_tool_resume") {
            let (toks, rest) = split_head(line, 2);
            if toks.len() >= 2 && !rest.is_empty() {
                resumes.push((toks[1].clone(), unquote(&rest)));
            }
            continue;
        }
        if line.starts_with("cc_tool") {
            let (toks, rest) = split_head(line, 3);
            if toks.len() < 3 || rest.is_empty() {
                continue;
            }
            let (cmd, prefix) = (toks[1].clone(), toks[2].clone());
            if seen.contains(&prefix) {
                continue;
            }
            seen.push(prefix.clone());
            out.push(Tool::new(&cmd, &prefix, &unquote(&rest)));
        }
    }

    // Apply declared extra-dir / resume metadata to matching tools.
    for (key, flag, max) in extra {
        for t in out.iter_mut() {
            if t.cmd == key || t.prefix == key {
                t.extra_flag = Some(flag.clone());
                t.extra_max = max;
            }
        }
    }
    for (key, suffix) in resumes {
        for t in out.iter_mut() {
            if t.cmd == key || t.prefix == key {
                t.resume_suffix = Some(suffix.clone());
                t.resume_keep_extra = true;
            }
        }
    }
    for (key, flag) in yolos {
        for t in out.iter_mut() {
            if t.cmd == key || t.prefix == key {
                t.yolo_flag = Some(flag.clone());
            }
        }
    }
    for (key, hint) in installs {
        for t in out.iter_mut() {
            if t.cmd == key || t.prefix == key {
                t.install_hint = Some(hint.clone());
            }
        }
    }
    for (key, cmd) in updates {
        for t in out.iter_mut() {
            if t.cmd == key || t.prefix == key {
                t.update_cmd = Some(cmd.clone());
            }
        }
    }
    for (key, flag) in rc_offs {
        for t in out.iter_mut() {
            if t.cmd == key || t.prefix == key {
                t.remote_off_flag = Some(flag.clone());
            }
        }
    }
    for (key, flag) in rc_ons {
        for t in out.iter_mut() {
            if t.cmd == key || t.prefix == key {
                t.remote_on_flag = Some(flag.clone());
            }
        }
    }
    out
}

/// Fill in the built-in extra-dir + resume support for any tool that didn't
/// declare its own (the four known CLIs). `config_dir` is only used to resolve
/// the 0082 remote-control disable file's absolute path.
fn with_defaults(mut tools: Vec<Tool>, config_dir: &Path) -> Vec<Tool> {
    for t in tools.iter_mut() {
        if t.extra_flag.is_none() {
            match (t.prefix.as_str(), t.cmd.as_str()) {
                ("claude" | "kimi" | "codex", _) | (_, "cc" | "kc" | "coc") => {
                    t.extra_flag = Some("--add-dir".into());
                }
                ("gemini", _) | (_, "gc") => {
                    t.extra_flag = Some("--include-directories".into());
                    t.extra_max = 5;
                }
                _ => {}
            }
        }
        if t.resume_suffix.is_none() {
            match (t.prefix.as_str(), t.cmd.as_str()) {
                ("claude" | "kimi", _) | (_, "cc" | "kc") => {
                    t.resume_suffix = Some("--continue".into());
                    t.resume_keep_extra = true;
                }
                ("gemini", _) | (_, "gc") => {
                    t.resume_suffix = Some("--resume latest".into());
                    t.resume_keep_extra = true;
                }
                ("codex", _) | (_, "coc") => {
                    t.resume_suffix = Some("resume --last".into());
                    t.resume_keep_extra = false; // codex resume rejects --add-dir
                }
                ("opencode", _) | (_, "oc") => {
                    t.resume_suffix = Some("--continue".into());
                    t.resume_keep_extra = false;
                }
                ("grok", _) | (_, "gk") => {
                    t.resume_suffix = Some("--continue".into());
                    t.resume_keep_extra = false;
                }
                _ => {}
            }
        }
        if t.yolo_flag.is_none() {
            let cand: Option<&str> = match (t.prefix.as_str(), t.cmd.as_str()) {
                ("claude", _) | (_, "cc") => Some("--dangerously-skip-permissions"),
                ("kimi", _) | (_, "kc") => Some("-y"),
                ("gemini", _) | (_, "gc") => Some("-y"),
                ("codex", _) | (_, "coc") => Some("--dangerously-bypass-approvals-and-sandbox"),
                ("opencode", _) | (_, "oc") => Some("--auto"),
                ("grok", _) | (_, "gk") => Some("--always-approve"),
                _ => None,
            };
            // Adopt the built-in flag only if the template doesn't already inline
            // it. A pre-0005 user template that bakes the flag in keeps
            // yolo_flag = None, so skip_permissions=false leaves it YOLO (we don't
            // strip inlined flags) and skip=true won't double it. (0005 §tools.conf.)
            if let Some(flag) = cand {
                if !t.tmpl.split_whitespace().any(|tok| tok == flag) {
                    t.yolo_flag = Some(flag.into());
                }
            }
        }
        // Image-paste delivery (0066): Codex — including a tools.conf override
        // that keeps its command or prefix — gets the bracketed-path strategy;
        // everything else (Claude, kimi, gemini, shell, unknown custom tools)
        // keeps the backward-compatible clipboard probe.
        if matches!((t.prefix.as_str(), t.cmd.as_str()), ("codex" | "opencode", _) | (_, "coc" | "oc")) {
            t.image_paste = ImagePasteStrategy::BracketedImagePath;
        }
        // The assistant's own remote control (0082). Claude Code is the only CLI
        // with a launch-time enable today: Codex's `remote-control` daemon is
        // user-invoked and its feature flag reports `removed`; gemini and kimi
        // have no analogous feature. The identity match is the usual `(prefix,
        // cmd)` one, so a tools.conf override of claude inherits the stance —
        // unless it already carries the feature by hand (the inline guard).
        if matches!((t.prefix.as_str(), t.cmd.as_str()), ("claude", _) | (_, "cc"))
            && !inlines_remote_control(&t.tmpl)
        {
            if t.remote_off_flag.is_none() {
                t.remote_off_flag =
                    Some(format!("--settings {}", shell_quote(&remote_off_path(config_dir).to_string_lossy())));
            }
            if t.remote_on_flag.is_none() {
                t.remote_on_flag = Some("--rc claude-{name}".into());
            }
        }
        // An explicitly declared empty flag (`cc_tool_remote_off claude ""`) is
        // the operator saying "append nothing here" — normalise it to None now
        // that the default-fill above has had its chance to see it as declared.
        if t.remote_off_flag.as_deref() == Some("") {
            t.remote_off_flag = None;
        }
        if t.remote_on_flag.as_deref() == Some("") {
            t.remote_on_flag = None;
        }
    }
    tools
}

/// Quote a single argument for the launch shell (`launch_shell`). POSIX
/// single-quote on Unix; on Windows (`cmd.exe /C`) wrap in double quotes and
/// double any embedded double-quote, which is enough for the directory paths we
/// pass through `--add-dir`.
#[cfg(not(windows))]
pub fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}
#[cfg(windows)]
pub fn shell_quote(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}

fn append_extra_dirs(mut cmd: String, t: &Tool, extra_dirs: &[String]) -> String {
    if let Some(flag) = &t.extra_flag {
        for dir in extra_dirs {
            cmd.push(' ');
            cmd.push_str(&shell_quote(flag));
            cmd.push(' ');
            cmd.push_str(&shell_quote(dir));
        }
    }
    cmd
}

fn launch_command(t: &Tool, name: &str, extra_dirs: &[String]) -> String {
    let cmd = t.tmpl.replace("{name}", name);
    append_extra_dirs(cmd, t, extra_dirs)
}

fn resume_command(t: &Tool, name: &str, extra_dirs: &[String]) -> Option<String> {
    let suffix = t.resume_suffix.as_ref()?;
    let mut cmd = t.tmpl.replace("{name}", name);
    if !suffix.is_empty() {
        cmd.push(' ');
        cmd.push_str(suffix);
    }
    if t.resume_keep_extra {
        cmd = append_extra_dirs(cmd, t, extra_dirs);
    }
    Some(cmd)
}

/// Build the shell command line for a session. Unlike the Go build there is no
/// "; tmux kill-session" tail — we observe the child's exit directly. When
/// `resume` is set we run "(resume) || (launch)" so a tool with nothing to
/// continue (or a rejected resume flag) still falls back to a fresh session.
///
/// `skip_permissions` (0005) gates the tool's `yolo_flag`: when true (the
/// default) and the tool has a flag, it's appended to *both* the fresh and
/// resume forms (so resuming stays YOLO too); when false the flag is omitted and
/// the CLI launches with its normal approval prompts.
///
/// `remote_control` (0082) gates the assistant's *own* remote control the same
/// way, except that **both** states carry a flag: off (the default) appends
/// `remote_off_flag` — the `--settings` disable file — and on appends
/// `remote_on_flag` (`--rc claude-{name}`, with `{name}` substituted like the
/// template's). Either may be `None`, in which case that launch is *stance-free*
/// and the user's own assistant settings govern. Both land on both branches of
/// the `(resume) || (fresh)` fallback, so resuming can't re-register a session
/// that was created with the switch off.
#[allow(clippy::too_many_arguments)]
pub fn build_launch(
    t: &Tool,
    name: &str,
    extra_dirs: &[String],
    resume: bool,
    skip_permissions: bool,
    remote_control: bool,
) -> String {
    let yolo = if skip_permissions { t.yolo_flag.as_deref() } else { None };
    // 0082: every launch of a remote-control-capable assistant carries an
    // explicit stance — disable by default, enable by choice.
    let rc = if remote_control { t.remote_on_flag.as_deref() } else { t.remote_off_flag.as_deref() };
    let with_flags = |mut cmd: String| {
        if let Some(flag) = yolo {
            cmd.push(' ');
            cmd.push_str(flag);
        }
        if let Some(flag) = rc {
            cmd.push(' ');
            // Flag-side `{name}` substitution (the template's own is done in
            // `launch_command`/`resume_command`) — `--rc claude-{name}`.
            cmd.push_str(&flag.replace("{name}", name));
        }
        cmd
    };
    let launch = with_flags(launch_command(t, name, extra_dirs));
    if resume {
        if let Some(rc) = resume_command(t, name, extra_dirs).map(with_flags) {
            if rc != launch {
                return format!("({rc}) || ({launch})");
            }
        }
    }
    launch
}

/// tmux-safe short name: collapse anything outside [A-Za-z0-9_-] to '-' and trim.
pub fn sanitize_name(s: &str) -> String {
    let mut out = String::new();
    let mut prev_dash = false;
    for ch in s.trim().chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
            out.push(ch);
            prev_dash = ch == '-';
        } else if !prev_dash {
            out.push('-');
            prev_dash = true;
        }
    }
    out.trim_matches('-').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fixed config dir for the registry under test. The 0082 disable-file
    /// path is baked into `remote_off_flag` at load time, so the tests compute
    /// their expectations from the same helper rather than hard-coding a path.
    fn test_cfg() -> PathBuf {
        PathBuf::from("/cfg")
    }

    /// The exact argument a default (switch-off) claude launch must carry.
    fn remote_off_arg() -> String {
        format!("--settings {}", shell_quote(&remote_off_path(&test_cfg()).to_string_lossy()))
    }

    #[test]
    fn builtin_defaults() {
        let t = load_tools(None, &test_cfg());
        let claude = t.iter().find(|x| x.prefix == "claude").unwrap();
        assert_eq!(claude.resume_suffix.as_deref(), Some("--continue"));
        assert_eq!(claude.extra_flag.as_deref(), Some("--add-dir"));
        let gem = t.iter().find(|x| x.prefix == "gemini").unwrap();
        assert_eq!(gem.extra_max, 5);
        let codex = t.iter().find(|x| x.prefix == "codex").unwrap();
        assert_eq!(codex.resume_suffix.as_deref(), Some("resume --last"));
        assert!(!codex.resume_keep_extra); // codex resume rejects --add-dir
        let opencode = t.iter().find(|x| x.prefix == "opencode").unwrap();
        assert_eq!(opencode.cmd, "oc");
        assert_eq!(opencode.resume_suffix.as_deref(), Some("--continue"));
        assert!(opencode.extra_flag.is_none());
        assert!(opencode.remote_on_flag.is_none());
        let grok = t.iter().find(|x| x.prefix == "grok").unwrap();
        assert_eq!(grok.cmd, "gk");
        assert_eq!(grok.resume_suffix.as_deref(), Some("--continue"));
        assert!(grok.extra_flag.is_none());
        assert!(grok.remote_on_flag.is_none());
        assert!(!grok.resume_keep_extra);
    }

    #[test]
    fn yolo_flag_split_out_of_templates() {
        // The dangerous flag lives in yolo_flag, never inline in the template.
        let t = load_tools(None, &test_cfg());
        let want = [
            ("claude", "--dangerously-skip-permissions"),
            ("kimi", "-y"),
            ("gemini", "-y"),
            ("codex", "--dangerously-bypass-approvals-and-sandbox"),
            ("opencode", "--auto"),
            ("grok", "--always-approve"),
        ];
        for (prefix, flag) in want {
            let tool = t.iter().find(|x| x.prefix == prefix).unwrap();
            assert_eq!(tool.yolo_flag.as_deref(), Some(flag), "{prefix} yolo_flag");
            assert!(!tool.tmpl.contains(flag), "{prefix} tmpl must not inline the flag");
        }
        // Gemini keeps --skip-trust (a trust-store convenience, not the bypass).
        let gem = t.iter().find(|x| x.prefix == "gemini").unwrap();
        assert!(gem.tmpl.contains("--skip-trust"));
        // The bare shell has no approval flag.
        let sh = t.iter().find(|x| x.prefix == "shell").unwrap();
        assert!(sh.yolo_flag.is_none());
    }

    #[test]
    fn image_paste_strategy_mapping() {
        // Codex and OpenCode use durable bracketed image paths.
        let t = load_tools(None, &test_cfg());
        for tool in &t {
            let want = if matches!(tool.prefix.as_str(), "codex" | "opencode") {
                ImagePasteStrategy::BracketedImagePath
            } else {
                ImagePasteStrategy::ClipboardProbe
            };
            assert_eq!(tool.image_paste, want, "{}", tool.prefix);
        }
        // A tools.conf override that keeps Codex's prefix or command inherits
        // its strategy through the same identity logic as the other defaults.
        let by_prefix = with_defaults(parse("cc_tool xx codex 'my-codex-wrapper'"), &test_cfg());
        assert_eq!(by_prefix[0].image_paste, ImagePasteStrategy::BracketedImagePath);
        let by_cmd = with_defaults(parse("cc_tool coc oai 'codex --profile fast'"), &test_cfg());
        assert_eq!(by_cmd[0].image_paste, ImagePasteStrategy::BracketedImagePath);
        let open = with_defaults(parse("cc_tool xx opencode 'my-opencode-wrapper'"), &test_cfg());
        assert_eq!(open[0].image_paste, ImagePasteStrategy::BracketedImagePath);
        // A genuinely unknown custom tool keeps the compatible probe.
        let custom = with_defaults(parse("cc_tool yy other 'other-cli'"), &test_cfg());
        assert_eq!(custom[0].image_paste, ImagePasteStrategy::ClipboardProbe);
    }

    #[test]
    fn build_launch_gates_yolo_flag() {
        let t = load_tools(None, &test_cfg());
        let claude = t.iter().find(|x| x.prefix == "claude").unwrap();
        // skip on (today's default): the flag is present.
        let yolo = build_launch(claude, "proj", &[], false, true, false);
        assert!(yolo.contains("--dangerously-skip-permissions"), "{yolo}");
        // skip off: launches with normal approval prompts — no flag.
        let safe = build_launch(claude, "proj", &[], false, false, false);
        assert!(!safe.contains("--dangerously-skip-permissions"), "{safe}");
        // Resume keeps the YOLO flag on both the resume and fallback forms.
        let resumed = build_launch(claude, "proj", &[], true, true, false);
        assert_eq!(resumed.matches("--dangerously-skip-permissions").count(), 2, "{resumed}");
        // …and drops it from both when skip is off.
        let resumed_safe = build_launch(claude, "proj", &[], true, false, false);
        assert!(!resumed_safe.contains("--dangerously-skip-permissions"), "{resumed_safe}");
    }

    #[test]
    fn opencode_launch_contract_is_exact() {
        let t = load_tools(None, &test_cfg());
        let tool = t.iter().find(|x| x.prefix == "opencode").unwrap();
        assert_eq!(build_launch(tool, "proj", &[], false, false, false), "opencode");
        assert_eq!(build_launch(tool, "proj", &[], false, true, false), "opencode --auto");
        assert_eq!(build_launch(tool, "proj", &[], true, false, false), "(opencode --continue) || (opencode)");
        let resumed = build_launch(tool, "proj", &[], true, true, false);
        assert_eq!(resumed, "(opencode --continue --auto) || (opencode --auto)");
        assert_eq!(resumed.matches("--auto").count(), 2);
    }

    #[test]
    fn grok_launch_contract_is_exact() {
        let t = load_tools(None, &test_cfg());
        let tool = t.iter().find(|x| x.prefix == "grok").unwrap();
        assert_eq!(build_launch(tool, "proj", &[], false, false, false), "grok");
        assert_eq!(build_launch(tool, "proj", &[], false, true, false), "grok --always-approve");
        assert_eq!(build_launch(tool, "proj", &[], true, false, false), "(grok --continue) || (grok)");
        let resumed = build_launch(tool, "proj", &[], true, true, false);
        assert_eq!(resumed, "(grok --continue --always-approve) || (grok --always-approve)");
        assert_eq!(resumed.matches("--always-approve").count(), 2);
        assert!(!resumed.contains("--yolo"), "do not bake the hidden alias: {resumed}");
        let by_prefix = with_defaults(parse("cc_tool xx grok 'my-grok-wrapper'"), &test_cfg());
        assert_eq!(by_prefix[0].yolo_flag.as_deref(), Some("--always-approve"));
        assert_eq!(by_prefix[0].resume_suffix.as_deref(), Some("--continue"));
        let by_cmd = with_defaults(parse("cc_tool gk oai 'grok --profile fast'"), &test_cfg());
        assert_eq!(by_cmd[0].yolo_flag.as_deref(), Some("--always-approve"));
    }

    #[test]
    fn cc_tool_yolo_declared_and_inline_guard() {
        // Explicit cc_tool_yolo wins.
        let cfg = "cc_tool xx myc 'myc run'\ncc_tool_yolo myc --go-fast";
        let t = parse(cfg);
        let t = with_defaults(t, &test_cfg());
        let tool = t.iter().find(|x| x.prefix == "myc").unwrap();
        assert_eq!(tool.yolo_flag.as_deref(), Some("--go-fast"));
        let line = build_launch(tool, "p", &[], false, true, false);
        assert!(line.ends_with("--go-fast"), "{line}");
        // A pre-0005 template that inlines the known flag keeps yolo_flag = None,
        // so skip=false can't strip it (stays YOLO) and skip=true won't double it.
        let inlined = with_defaults(
            parse("cc_tool cc claude \"claude --dangerously-skip-permissions\""),
            &test_cfg(),
        );
        let c = inlined.iter().find(|x| x.prefix == "claude").unwrap();
        assert!(c.yolo_flag.is_none(), "inlined flag must not adopt a default yolo_flag");
        let on = build_launch(c, "p", &[], false, true, false);
        assert_eq!(on.matches("--dangerously-skip-permissions").count(), 1, "{on}");
        let off = build_launch(c, "p", &[], false, false, false);
        assert!(off.contains("--dangerously-skip-permissions"), "can't strip inlined: {off}");
    }

    #[test]
    fn probe_binary_is_the_template_head() {
        // The binary a session needs is the first bare token of the launch
        // template — for every built-in and for a renamed tools.conf override.
        let t = load_tools(None, &test_cfg());
        for (prefix, bin) in [("claude", "claude"), ("kimi", "kimi"), ("gemini", "gemini"), ("codex", "codex"), ("opencode", "opencode"), ("grok", "grok")] {
            let tool = t.iter().find(|x| x.prefix == prefix).unwrap();
            assert_eq!(probe_binary(tool), Some(bin), "{prefix}");
        }
        // The shell tool's head is a shell expansion — always resolves, no probe.
        let sh = t.iter().find(|x| x.prefix == "shell").unwrap();
        assert_eq!(probe_binary(sh), None);
        // A tools.conf override that renames the binary probes the new name.
        let custom = with_defaults(parse("cc_tool cc claude \"my-claude-wrapper --fast\""), &test_cfg());
        let c = custom.iter().find(|x| x.prefix == "claude").unwrap();
        assert_eq!(probe_binary(c), Some("my-claude-wrapper"));
    }

    #[test]
    fn missing_binary_probes_the_given_path() {
        // Synthesized PATH: one dir holding a single executable.
        let dir = std::env::temp_dir().join(format!("cctools-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("fake-cli");
        std::fs::write(&exe, "#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let path = dir.to_str().unwrap();
        let present = Tool::new("fk", "fake", "fake-cli --go");
        assert_eq!(missing_binary(&present, path), None);
        let absent = Tool::new("no", "nope", "nope-cli");
        assert_eq!(missing_binary(&absent, path).as_deref(), Some("nope-cli"));
        // Shell tool: nothing to probe even on an empty PATH.
        let sh = Tool::new("tt", "shell", "${SHELL:-/bin/bash} -l");
        assert_eq!(missing_binary(&sh, ""), None);
        // A non-executable file is not a hit (mirrors the shell's lookup).
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let plain = dir.join("plain-file");
            std::fs::write(&plain, "data").unwrap();
            std::fs::set_permissions(&plain, std::fs::Permissions::from_mode(0o644)).unwrap();
            let t = Tool::new("pf", "plain", "plain-file");
            assert!(missing_binary(&t, path).is_some());
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A scratch dir with files created by name — used to express *Windows*
    /// on-disk shapes (`claude.exe`, `codex.cmd`) from a Linux test run.
    struct Scratch(std::path::PathBuf);
    impl Scratch {
        fn new(tag: &str) -> Scratch {
            let d = std::env::temp_dir().join(format!("ccr-res-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&d);
            std::fs::create_dir_all(&d).unwrap();
            Scratch(d)
        }
        fn file(&self, name: &str) -> PathBuf {
            let p = self.0.join(name);
            std::fs::write(&p, "x").unwrap();
            p
        }
        fn dir(&self) -> String {
            self.0.to_string_lossy().to_string()
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn windows_path_splits_on_semicolons_not_colons() {
        // 0051's bug 1: a ';'-separated PATH split on ':' yields fragments like
        // "C" and "\Users\erik\.local\bin;C" — never a directory.
        let r = Resolver::windows(".COM;.EXE");
        let entries = r.entries(r"C:\a\bin;C:\Program Files\nodejs;;C:\Users\erik\.local\bin");
        assert_eq!(entries, vec![r"C:\a\bin", r"C:\Program Files\nodejs", r"C:\Users\erik\.local\bin"]);
        // Quoted entries (legal on Windows) lose their quotes, as cmd.exe does.
        assert_eq!(Resolver::windows("").entries("\"C:\\a b\";C:\\c"), vec![r"C:\a b", r"C:\c"]);
        // Unix is unchanged.
        assert_eq!(Resolver::unix().entries("/usr/bin:/bin::/opt/x"), vec!["/usr/bin", "/bin", "/opt/x"]);
    }

    #[test]
    fn windows_resolves_through_pathext() {
        // 0051's bug 2: the file on disk is `claude.exe` / `codex.cmd`, never the
        // bare name — and `cmd.exe /C` is what actually launches the session.
        let s = Scratch::new("pathext");
        s.file("claude.exe");
        s.file("codex.cmd");
        s.file("only.ps1");
        let dir = s.dir();
        // A real `PATHEXT` is upper-case and a real Windows filesystem is
        // case-insensitive, so `claude` + `.EXE` finds `claude.exe` there. This
        // test runs on a case-SENSITIVE host, so it spells the same list in the
        // case the files use — the resolver deliberately plays no case games
        // (`with_exts` appends the candidate exactly as given).
        let pathext = ".com;.exe;.bat;.cmd;.vbs";
        let r = Resolver::windows(pathext);
        assert_eq!(r.resolve("claude", &dir).unwrap(), s.0.join("claude.exe"));
        assert_eq!(r.resolve("codex", &dir).unwrap(), s.0.join("codex.cmd"));
        // A head that already carries its extension resolves as itself — it must
        // not become `claude.exe.EXE`.
        assert_eq!(r.resolve("claude.exe", &dir).unwrap(), s.0.join("claude.exe"));
        // PATHEXT is FOLLOWED, not guessed: `.PS1` isn't listed here, and
        // `cmd.exe /C only` wouldn't run it either, so we don't claim it.
        assert!(r.resolve("only", &dir).is_none());
        assert!(r.resolve("nope", &dir).is_none());
        // An unset PATHEXT falls back to cmd.exe's own documented default set —
        // and the bare name is always the first candidate.
        let dflt = Resolver::windows("");
        assert_eq!(dflt.exts[0], "");
        assert!(dflt.exts.iter().any(|e| e == ".EXE") && dflt.exts.iter().any(|e| e == ".CMD"));
        // A bare entry ("EXE", no dot) is normalised, and duplicates collapse
        // case-insensitively.
        let odd = Resolver::windows("EXE;.exe;.CMD");
        assert_eq!(odd.exts, vec!["".to_string(), ".EXE".to_string(), ".CMD".to_string()]);
    }

    #[test]
    fn absolute_and_drive_heads_take_the_direct_branch() {
        // 0051's bug 3: `contains('/')` is POSIX-only, so `C:\tools\claude.exe`
        // took the search-the-PATH branch and never matched.
        let s = Scratch::new("abs");
        let exe = s.file("claude.exe");
        let win = Resolver::windows(".EXE");
        let head = exe.to_string_lossy().replace('/', "\\");
        assert!(win.is_path(&head), "{head} must read as a path");
        assert!(win.is_path(r"C:\tools\claude.exe"));
        assert!(win.is_path(r"tools\claude.exe"));
        assert!(!win.is_path("claude"));
        // …and on Unix an absolute head is checked directly, with an empty PATH.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let bin = s.file("direct");
            std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
            let u = Resolver::unix();
            assert!(u.is_path(bin.to_str().unwrap()));
            assert_eq!(u.resolve(bin.to_str().unwrap(), "").unwrap(), bin);
        }
    }

    #[test]
    fn unix_resolution_is_byte_for_byte_unchanged() {
        // The Unix contract: ':'-separated, the executable bit is the test, no
        // extensions are ever appended.
        let s = Scratch::new("unix");
        let exe = s.file("fake-cli");
        s.file("plain");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let r = Resolver::unix();
        assert_eq!(r.exts, vec![String::new()], "no extension candidates on Unix");
        let path = format!("/nonexistent:{}", s.dir());
        #[cfg(unix)]
        {
            assert_eq!(r.resolve("fake-cli", &path).unwrap(), exe);
            assert!(r.resolve("plain", &path).is_none(), "a non-executable file is not a hit");
        }
        assert!(r.resolve("nope", &path).is_none());
        assert!(r.resolve("", &path).is_none());
    }

    #[test]
    fn cc_tool_install_declares_a_hint() {
        // A custom tool can declare its own install one-liner (0046), matched by
        // cmd or prefix like the other cc_tool_* metadata lines.
        let cfg = "cc_tool xx myc 'myc run'\ncc_tool_install myc \"cargo install myc\"";
        let t = with_defaults(parse(cfg), &test_cfg());
        let tool = t.iter().find(|x| x.prefix == "myc").unwrap();
        assert_eq!(tool.install_hint.as_deref(), Some("cargo install myc"));
        // The declared hint wins over the built-in registry…
        assert_eq!(install_hint(tool).as_deref(), Some("cargo install myc"));
        // …the built-ins fall back to the assistant registry…
        let builtins = load_tools(None, &test_cfg());
        let claude = builtins.iter().find(|x| x.prefix == "claude").unwrap();
        assert!(install_hint(claude).is_some(), "built-in claude must carry an install hint");
        // …and a custom tool with no declaration has none (guard still reports).
        let bare = with_defaults(parse("cc_tool yy other 'other-cli'"), &test_cfg());
        let b = bare.iter().find(|x| x.prefix == "other").unwrap();
        assert_eq!(install_hint(b), None);
    }

    #[test]
    fn cc_tool_update_wins_over_the_registry() {
        // A declared update command is the first (and for a custom tool, only)
        // candidate — the escape hatch for a pin/wrapper/distro package (0049).
        let cfg = "cc_tool xx myc 'myc run'\ncc_tool_update myc \"cargo install myc --locked\"";
        let t = with_defaults(parse(cfg), &test_cfg());
        let tool = t.iter().find(|x| x.prefix == "myc").unwrap();
        assert_eq!(tool.update_cmd.as_deref(), Some("cargo install myc --locked"));
        assert_eq!(update_commands(tool), vec!["cargo install myc --locked".to_string()]);
        // A custom tool with no declaration and no descriptor has no known path —
        // reported as "skipped", never an error.
        let bare = with_defaults(parse("cc_tool yy other 'other-cli'"), &test_cfg());
        assert!(update_commands(bare.iter().find(|x| x.prefix == "other").unwrap()).is_empty());
        // An override on a built-in still wins, with the registry's commands after.
        let over = with_defaults(
            parse("cc_tool cc claude 'claude'\ncc_tool_update claude \"claude update --pin 2.1.220\""),
            &test_cfg(),
        );
        let c = over.iter().find(|x| x.prefix == "claude").unwrap();
        assert_eq!(update_commands(c)[0], "claude update --pin 2.1.220");
        assert!(update_commands(c).len() > 1, "the registry commands remain as fallbacks");
    }

    #[test]
    fn update_commands_order_self_update_then_package_manager() {
        let t = load_tools(None, &test_cfg());
        // claude/codex have their own `update` subcommand — tried first, because
        // the CLI knows how it was installed.
        for prefix in ["claude", "codex"] {
            let tool = t.iter().find(|x| x.prefix == prefix).unwrap();
            let cmds = update_commands(tool);
            assert_eq!(cmds[0], format!("{prefix} update"), "{prefix} self-update first");
            assert_eq!(cmds.len(), 2, "…with the package-manager fallback after: {cmds:?}");
        }
        // gemini/kimi have none (verified) — straight to the package manager.
        let gem = update_commands(t.iter().find(|x| x.prefix == "gemini").unwrap());
        assert_eq!(gem.len(), 1);
        assert!(gem[0].contains("gemini-cli"), "{gem:?}");
        let kimi = update_commands(t.iter().find(|x| x.prefix == "kimi").unwrap());
        assert_eq!(kimi.len(), 1);
        assert!(kimi[0].contains("kimi-cli"), "{kimi:?}");
        let grok = update_commands(t.iter().find(|x| x.prefix == "grok").unwrap());
        assert_eq!(grok, vec!["grok update".to_string()]);
        let open = update_commands(t.iter().find(|x| x.prefix == "opencode").unwrap());
        assert_eq!(open, vec!["opencode upgrade".to_string()]);
        // The bare shell is not an assistant — nothing to update, ever.
        let sh = t.iter().find(|x| x.prefix == "shell").unwrap();
        assert!(update_commands(sh).is_empty());
        assert_eq!(version_arg(sh), "--version"); // harmless default; never probed
    }

    #[test]
    fn every_assistant_maps_to_a_default_tool() {
        // The registry and the tool defaults must not drift: each assistant
        // descriptor describes exactly one built-in tool, with install commands
        // for both supported platforms.
        let t = load_tools(None, &test_cfg());
        for a in ASSISTANTS {
            let tool = t
                .iter()
                .find(|x| x.prefix == a.prefix)
                .unwrap_or_else(|| panic!("assistant '{}' has no default tool", a.prefix));
            assert!(probe_binary(tool).is_some(), "{} must be probeable", a.prefix);
            assert!(!a.install_macos.is_empty() && !a.install_linux.is_empty() && !a.docs.is_empty());
            // …on all three platforms (0050 Part H): Windows is no longer a
            // "see <docs>" hole, which is what made install/update no-ops there.
            assert!(!a.install_windows.is_empty(), "{} install_windows", a.prefix);
            assert!(!a.self_update.is_empty() || !a.update_windows.is_empty(), "{} update_windows", a.prefix);
            // …with a size hint for the dialog, and every declared prerequisite
            // resolving to a real PREREQS entry (a typo'd key would silently
            // install nothing and then blame the assistant).
            assert!(!a.size_hint.is_empty(), "{} size_hint", a.prefix);
            for key in a.needs_macos.iter().chain(a.needs_linux.iter()).chain(a.needs_windows.iter()) {
                assert!(prereq(key).is_some(), "{} needs unknown prerequisite '{key}'", a.prefix);
            }
            // …and (0049) a way to update it + read its version on both platforms.
            assert!(!a.self_update.is_empty() || (!a.update_macos.is_empty() && !a.update_linux.is_empty()), "{} update cmd", a.prefix);
            assert!(!a.version_arg.is_empty(), "{} version_arg", a.prefix);
            assert!(!update_commands(tool).is_empty(), "{} must have an update path", a.prefix);
        }
        // And the shell tool deliberately has no descriptor.
        assert!(!ASSISTANTS.iter().any(|a| a.prefix == "shell"));
    }

    #[test]
    fn prerequisites_are_declared_and_a_cc_tool_install_suppresses_them() {
        let t = load_tools(None, &test_cfg());
        let need = |prefix: &str| {
            prereqs_for(t.iter().find(|x| x.prefix == prefix).unwrap())
                .into_iter()
                .map(|p| p.key)
                .collect::<Vec<_>>()
        };
        // The declared shape (0050 Part A), verified against a working machine.
        assert_eq!(need("codex"), vec!["npm"]);
        assert_eq!(need("gemini"), vec!["npm"]);
        assert_eq!(need("kimi"), vec!["uv"]);
        let open = t.iter().find(|x| x.prefix == "opencode").unwrap();
        assert!(prereqs_for_platform(open, AssistantPlatform::Linux).is_empty());
        assert_eq!(prereqs_for_platform(open, AssistantPlatform::Windows).iter().map(|p| p.key).collect::<Vec<_>>(), vec!["npm"]);
        let grok = t.iter().find(|x| x.prefix == "grok").unwrap();
        assert!(prereqs_for_platform(grok, AssistantPlatform::Linux).is_empty());
        assert!(prereqs_for_platform(grok, AssistantPlatform::Macos).is_empty());
        assert!(prereqs_for_platform(grok, AssistantPlatform::Windows).is_empty());
        // Claude's installer is self-contained (curl + bash).
        assert!(need("claude").is_empty());
        // The bare shell isn't an assistant — nothing to install, ever.
        assert!(need("shell").is_empty());
        assert!(install_command(t.iter().find(|x| x.prefix == "shell").unwrap()).is_none());

        // A machine that declares its own install line short-circuits the
        // prerequisite logic entirely: the self-hoster said what they wanted, and
        // rewriting someone's config would be worse than trusting it.
        let custom = with_defaults(
            parse("cc_tool coc codex 'codex'\ncc_tool_install codex \"my-mirror add codex\""),
            &test_cfg(),
        );
        let c = custom.iter().find(|x| x.prefix == "codex").unwrap();
        assert_eq!(install_command(c).as_deref(), Some("my-mirror add codex"));
        assert!(prereqs_for(c).is_empty(), "an explicit install line owns its own deps");

        // Every prerequisite is user-scope on at least the two Unix platforms,
        // and none of them escalates.
        for p in PREREQS {
            assert!(!p.install_macos.is_empty() && !p.install_linux.is_empty(), "{} unix install", p.key);
            assert!(!p.docs.is_empty() && !p.bin.is_empty() && !p.label.is_empty());
            assert!(!p.size_hint.is_empty(), "{} size_hint", p.key);
        }
    }

    #[test]
    fn install_command_is_runnable_but_install_hint_may_be_prose() {
        // The distinction 0050 B2 depends on: `install_hint` can return a
        // "see <docs>" DISPLAY string, which must never reach a shell. A custom
        // tool with no descriptor and no declaration has neither.
        let bare = with_defaults(parse("cc_tool yy other 'other-cli'"), &test_cfg());
        let b = bare.iter().find(|x| x.prefix == "other").unwrap();
        assert_eq!(install_command(b), None);
        assert_eq!(install_hint(b), None);
        // A built-in always has a runnable command on every supported platform
        // now, so the two agree — the "see docs" branch is the fallback for a
        // future assistant that lacks a per-OS line.
        let t = load_tools(None, &test_cfg());
        for prefix in ["claude", "codex", "gemini", "kimi", "opencode"] {
            let tool = t.iter().find(|x| x.prefix == prefix).unwrap();
            let cmd = install_command(tool).expect(prefix);
            assert!(!cmd.starts_with("see "), "{prefix}: {cmd}");
            assert_eq!(install_hint(tool).as_deref(), Some(cmd.as_str()));
            assert!(!size_hint(tool).is_empty(), "{prefix} size hint");
        }
    }

    #[test]
    fn sanitize() {
        assert_eq!(sanitize_name(" my proj!! "), "my-proj");
        assert_eq!(sanitize_name("a/b"), "a-b");
        assert_eq!(sanitize_name("ok_name-1"), "ok_name-1");
    }

    #[test]
    fn launch_and_resume_fallback() {
        // The built-in claude template is a plain `claude` (0015) — no `--rc`.
        let t = with_defaults(defaults(), &test_cfg())
            .into_iter()
            .find(|t| t.prefix == "claude")
            .unwrap();
        // skip_permissions=false here keeps these assertions focused on the
        // launch/resume shape (yolo gating is covered by build_launch_gates_yolo_flag).
        // The fresh form is no longer a bare `claude`: since 0082 every launch
        // carries an explicit remote-control stance, and the default one is the
        // `--settings` disable file. `--rc` is still absent — that's the point.
        let fresh = build_launch(&t, "proj", &[], false, false, false);
        assert_eq!(fresh, format!("claude {}", remote_off_arg()));
        let resumed = build_launch(&t, "proj", &[], true, false, false);
        assert!(resumed.contains("--continue"));
        assert!(resumed.contains("||")); // (resume) || (fresh) fallback
        assert!(!resumed.contains("--rc"), "default no longer registers with the desktop app: {resumed}");
        let with_extra = build_launch(&t, "proj", &["/home/u/lib".to_string()], false, false, false);
        assert!(with_extra.contains("--add-dir") && with_extra.contains("/home/u/lib"));
    }

    #[test]
    fn rc_optin_substitutes_name() {
        // Opting back into desktop registration via a tools.conf override
        // (`cc_tool cc claude "claude --rc 'claude-{name}'"`) still substitutes
        // {name} → the session name, so the `{name}` launch logic stays covered.
        let t = with_defaults(parse("cc_tool cc claude \"claude --rc 'claude-{name}'\""), &test_cfg())
            .into_iter()
            .find(|t| t.prefix == "claude")
            .unwrap();
        let fresh = build_launch(&t, "proj", &[], false, false, false);
        assert_eq!(fresh, "claude --rc 'claude-proj'");
        // Resume still appends --continue ahead of the registered launch.
        let resumed = build_launch(&t, "proj", &[], true, false, false);
        assert!(resumed.contains("--continue") && resumed.contains("--rc"), "{resumed}");
    }

    // ── The assistant's own remote control (proposal 0082) ───────────────────

    #[test]
    fn build_launch_gates_the_remote_control_stance() {
        let t = load_tools(None, &test_cfg());
        let claude = t.iter().find(|x| x.prefix == "claude").unwrap();
        // Off (the default): the disable settings file, quoted for the launch
        // shell — single quotes on Unix, double quotes under `cmd.exe /C`.
        let off = build_launch(claude, "proj", &[], false, false, false);
        assert_eq!(off, format!("claude {}", remote_off_arg()));
        #[cfg(not(windows))]
        assert!(off.contains("'/cfg/claude-remote-off.json'"), "{off}");
        #[cfg(windows)]
        assert!(off.contains("\"") && off.contains("claude-remote-off.json"), "{off}");
        assert!(!off.contains("--rc"), "{off}");
        // On: registration under the cc-screen-derived name, with the flag-side
        // {name} substituted (new in 0082 — only templates were substituted).
        let on = build_launch(claude, "proj", &[], false, false, true);
        assert_eq!(on, "claude --rc claude-proj");
        assert!(!on.contains("--settings"), "an opted-in launch carries no disable file: {on}");
        // Both stances ride BOTH branches of the (resume) || (fresh) fallback, so
        // resume can't re-register a session created with the switch off.
        let resumed_off = build_launch(claude, "proj", &[], true, false, false);
        assert_eq!(resumed_off.matches("--settings").count(), 2, "{resumed_off}");
        assert!(!resumed_off.contains("--rc"), "{resumed_off}");
        let resumed_on = build_launch(claude, "proj", &[], true, false, true);
        assert_eq!(resumed_on.matches("--rc claude-proj").count(), 2, "{resumed_on}");
        // The stance lands after the YOLO flag, and the two compose.
        let both = build_launch(claude, "proj", &[], false, true, false);
        assert_eq!(both, format!("claude --dangerously-skip-permissions {}", remote_off_arg()));
    }

    #[test]
    fn only_claude_carries_a_remote_control_stance() {
        // Codex's remote-control daemon is user-invoked and its feature flag
        // reports `removed`; gemini/kimi have no analogous feature; the bare
        // shell isn't an assistant. Their launch lines must be byte-identical to
        // pre-0082 — the acceptance criterion this test IS.
        let t = load_tools(None, &test_cfg());
        for (prefix, want) in [
            ("kimi", "kimi"),
            ("gemini", "gemini --skip-trust"),
            ("codex", "codex"),
        ] {
            let tool = t.iter().find(|x| x.prefix == prefix).unwrap();
            assert!(tool.remote_off_flag.is_none() && tool.remote_on_flag.is_none(), "{prefix}");
            assert_eq!(build_launch(tool, "proj", &[], false, false, false), want);
            // …and asking for "on" on a tool that has nothing to turn on is a
            // stance-free launch, never a stray flag.
            assert_eq!(build_launch(tool, "proj", &[], false, false, true), want);
            assert!(!tool_info(tool, false).remote_control_available, "{prefix}");
        }
        let sh = t.iter().find(|x| x.prefix == "shell").unwrap();
        assert!(sh.remote_off_flag.is_none() && sh.remote_on_flag.is_none());
        // Claude is the one capable tool, so it's the one that offers the switch.
        let claude = t.iter().find(|x| x.prefix == "claude").unwrap();
        assert!(tool_info(claude, false).remote_control_available);
    }

    #[test]
    fn remote_control_inline_guard_and_its_false_positive() {
        // A template that already carries the feature by hand is left entirely
        // alone: we neither strip, double, nor contradict it.
        for tmpl in [
            "cc_tool cc claude \"claude --rc 'claude-{name}'\"",
            "cc_tool cc claude \"claude --remote-control\"",
            "cc_tool cc claude \"claude --rc=mine\"",
            "cc_tool cc claude \"claude --remote-control=mine\"",
        ] {
            let t = with_defaults(parse(tmpl), &test_cfg());
            let c = t.iter().find(|x| x.prefix == "claude").unwrap();
            assert!(c.remote_off_flag.is_none(), "guard must fire: {tmpl}");
            assert!(c.remote_on_flag.is_none(), "guard must fire: {tmpl}");
            let line = build_launch(c, "p", &[], false, false, false);
            assert!(!line.contains("--settings /cfg"), "no stance appended: {line}");
        }
        // A template inlining the DISABLE key (inside a --settings argument) also
        // trips it — substring, because token equality could never see it.
        let disabled = with_defaults(
            parse("cc_tool cc claude \"claude --settings '{\\\"disableRemoteControl\\\": true}'\""),
            &test_cfg(),
        );
        let d = disabled.iter().find(|x| x.prefix == "claude").unwrap();
        assert!(d.remote_off_flag.is_none() && d.remote_on_flag.is_none());
        // …but `--remote-control-session-name-prefix` is naming-only and must NOT
        // trip it: a false positive there silently leaves those sessions
        // remote-control-capable.
        let named = with_defaults(
            parse("cc_tool cc claude \"claude --remote-control-session-name-prefix box\""),
            &test_cfg(),
        );
        let n = named.iter().find(|x| x.prefix == "claude").unwrap();
        assert_eq!(n.remote_off_flag.as_deref(), Some(remote_off_arg().as_str()));
        assert_eq!(n.remote_on_flag.as_deref(), Some("--rc claude-{name}"));
    }

    #[test]
    fn cc_tool_remote_directives_override_and_clear() {
        // An override wins over the built-in stance…
        let t = with_defaults(
            parse("cc_tool cc claude 'claude'\ncc_tool_remote_off claude \"--settings /etc/off.json\"\ncc_tool_remote_on claude \"--rc box-{name}\""),
            &test_cfg(),
        );
        let c = t.iter().find(|x| x.prefix == "claude").unwrap();
        assert_eq!(build_launch(c, "p", &[], false, false, false), "claude --settings /etc/off.json");
        assert_eq!(build_launch(c, "p", &[], false, false, true), "claude --rc box-p");
        // …and an explicitly empty flag is the stance-free escape hatch: nothing
        // appended, the user's own assistant settings govern.
        let cleared = with_defaults(
            parse("cc_tool cc claude 'claude'\ncc_tool_remote_off claude \"\""),
            &test_cfg(),
        );
        let c = cleared.iter().find(|x| x.prefix == "claude").unwrap();
        assert!(c.remote_off_flag.is_none());
        assert_eq!(build_launch(c, "p", &[], false, false, false), "claude");
        // Clearing one field leaves the other alone (they're independent), so a
        // session opted in on a cleared-on tool relaunches stance-free — and the
        // capability flag stops advertising a switch that would do nothing.
        assert!(c.remote_on_flag.is_some() && tool_info(c, false).remote_control_available);
        let no_on = with_defaults(
            parse("cc_tool cc claude 'claude'\ncc_tool_remote_on claude \"\""),
            &test_cfg(),
        );
        let c = no_on.iter().find(|x| x.prefix == "claude").unwrap();
        assert!(c.remote_on_flag.is_none());
        assert!(!tool_info(c, false).remote_control_available);
        assert_eq!(build_launch(c, "p", &[], false, false, true), "claude");
        // The directives reach a *custom* tool too — the day another CLI grows a
        // launch-time enable, it's a tools.conf line, not a code change.
        let custom = with_defaults(
            parse("cc_tool xx myc 'myc run'\ncc_tool_remote_on myc \"--register {name}\""),
            &test_cfg(),
        );
        let m = custom.iter().find(|x| x.prefix == "myc").unwrap();
        assert_eq!(build_launch(m, "p", &[], false, false, true), "myc run --register p");
    }

    #[test]
    fn remote_off_file_is_written_and_repaired() {
        let dir = std::env::temp_dir().join(format!("ccr-rc-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        // Created (with its parent) on first call…
        assert!(ensure_remote_off_file(&dir));
        let path = remote_off_path(&dir);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), REMOTE_OFF_JSON);
        // …idempotent…
        assert!(ensure_remote_off_file(&dir));
        // …and repaired if something wiped or clobbered it, so no launch ever
        // passes `--settings` at a dangling (or wrong) path.
        std::fs::remove_file(&path).unwrap();
        assert!(ensure_remote_off_file(&dir));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), REMOTE_OFF_JSON);
        std::fs::write(&path, "{}").unwrap();
        assert!(ensure_remote_off_file(&dir));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), REMOTE_OFF_JSON);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

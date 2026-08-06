//! Proposal 0060 Part A/C — persistence + subcommand tests that drive the REAL
//! `ccs` binary (env!(CARGO_BIN_EXE_ccs)) with `XDG_CONFIG_HOME` pointed at a
//! throwaway dir, in a dedicated test binary so the env juggling can't leak
//! into the parallel e2e suite (integration tests run as separate processes).

use std::path::{Path, PathBuf};
use std::process::Command;

use cc_screen_auth::Auth;
use cc_screen_hub::test_support::{start_hub, start_hub_multi};

fn ccs() -> &'static str {
    env!("CARGO_BIN_EXE_ccs")
}

/// A fresh XDG_CONFIG_HOME dir; `cfg_dir` is where cc-screen-tui's files land.
fn fresh_xdg(tag: &str) -> (PathBuf, PathBuf) {
    let xdg = std::env::temp_dir().join(format!("ccs-persist-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&xdg);
    std::fs::create_dir_all(&xdg).unwrap();
    (xdg.clone(), xdg.join("cc-screen-tui"))
}

/// Run the ccs binary. MUST go through spawn_blocking: these tests' in-process
/// hub lives on the (current-thread) tokio runtime, and a blocking
/// `Command::output()` on that thread would deadlock the child against the hub
/// it is talking to.
async fn run(xdg: &Path, args: &[&str]) -> std::process::Output {
    let xdg = xdg.to_path_buf();
    let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    tokio::task::spawn_blocking(move || {
        Command::new(ccs())
            .args(&args)
            .env("XDG_CONFIG_HOME", &xdg)
            // Make the browser auto-open heuristic always skip ("over SSH").
            .env("SSH_TTY", "/dev/pts/test")
            // No ambient tokens.
            .env_remove("CCS_API_TOKEN")
            .env_remove("CCWEB_API_TOKEN")
            .output()
            .expect("run ccs")
    })
    .await
    .unwrap()
}

/// A1 (acceptance 3): a virgin `ccs --server X` persists X — the recents-save
/// clobber that used to write localhost is dead.
#[tokio::test]
async fn explicit_server_is_genuinely_remembered() {
    let hub = start_hub(Auth::new(None, None, [1u8; 32]), &[]).await;
    let (xdg, cfg_dir) = fresh_xdg("a1");
    let url = format!("http://{hub}");

    // Direct-attach a nonexistent session: exits 1 pre-TTY, but the explicit
    // --server must already be saved by then.
    let out = run(&xdg, &["--server", &url, "no-such-session"]).await;
    assert_eq!(out.status.code(), Some(1), "not-found exits 1: {out:?}");

    let cfg = std::fs::read_to_string(cfg_dir.join("config.toml")).expect("config written");
    assert!(cfg.contains(&url), "config carries the explicit server, got:\n{cfg}");
    assert!(!cfg.contains("127.0.0.1:8839"), "no localhost clobber:\n{cfg}");
}

/// A2: a corrupt config warns (stderr, pre-alt-screen), is preserved as
/// config.toml.bad, and never silently overwritten.
#[tokio::test]
async fn corrupt_config_warns_and_is_kept_as_bad() {
    let hub = start_hub(Auth::new(None, None, [1u8; 32]), &[]).await;
    let (xdg, cfg_dir) = fresh_xdg("a2");
    std::fs::create_dir_all(&cfg_dir).unwrap();
    std::fs::write(cfg_dir.join("config.toml"), "server = [this is not toml").unwrap();

    let url = format!("http://{hub}");
    let out = run(&xdg, &["--server", &url, "no-such-session"]).await;
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("couldn't parse"), "warns about the corrupt file:\n{stderr}");

    let bad = std::fs::read_to_string(cfg_dir.join("config.toml.bad")).expect("original preserved");
    assert!(bad.contains("this is not toml"), "user-written bytes kept verbatim");
    let cfg = std::fs::read_to_string(cfg_dir.join("config.toml")).unwrap();
    assert!(cfg.contains(&url), "fresh config written with the explicit server");
}

/// C3 + acceptance 4: an explicit --token lands in credentials.toml (0600 in a
/// 0700 dir, keyed by host), NOT in config.toml.
#[tokio::test]
async fn explicit_token_lands_in_credentials_not_config() {
    let hub = start_hub(Auth::new(None, Some("sekrit-tok".into()), [1u8; 32]), &[]).await;
    let (xdg, cfg_dir) = fresh_xdg("c3");
    let url = format!("http://{hub}");

    let out = run(&xdg, &["--server", &url, "--token", "sekrit-tok", "no-such-session"]).await;
    assert_eq!(out.status.code(), Some(1), "{out:?}");

    let creds_path = cfg_dir.join("credentials.toml");
    let creds = std::fs::read_to_string(&creds_path).expect("credentials written");
    assert!(creds.contains("sekrit-tok"), "token stored:\n{creds}");
    assert!(creds.contains(&format!("servers.\"{hub}\"")), "keyed by host:\n{creds}");
    let cfg = std::fs::read_to_string(cfg_dir.join("config.toml")).unwrap();
    assert!(!cfg.contains("sekrit-tok"), "the token never lands in config.toml:\n{cfg}");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let fmode = std::fs::metadata(&creds_path).unwrap().permissions().mode() & 0o777;
        assert_eq!(fmode, 0o600, "credentials.toml is private");
        let dmode = std::fs::metadata(&cfg_dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(dmode, 0o700, "config dir is private");
    }
}

/// Acceptance 1/2-shaped, through the real binary: `ccs activate --server …`
/// prints the code, the "phone" approves, the sign-in persists, and the token
/// then authenticates REST as the right user. No TTY, no browser — SSH-shaped.
#[tokio::test]
async fn activate_via_binary_signs_in_and_persists() {
    let hub = start_hub_multi().await;
    let (_alice, cookie) = hub.user_with_cookie("alice@x.com").await;
    let (xdg, cfg_dir) = fresh_xdg("act");
    let url = format!("http://{}", hub.addr);

    // Browser side, concurrent with the child process.
    let approver = {
        let hub = hub.clone();
        tokio::spawn(async move {
            let code = hub.pending_user_code().await.expect("pending code");
            assert_eq!(hub.approve(&cookie, &code).await, 200);
        })
    };
    let url2 = url.clone();
    let out = tokio::task::spawn_blocking(move || {
        Command::new(ccs())
            .args(["activate", "--server", &url2])
            .env("XDG_CONFIG_HOME", &xdg)
            .env("SSH_TTY", "/dev/pts/test")
            .output()
            .expect("run ccs activate")
    })
    .await
    .unwrap();
    approver.await.unwrap();

    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(out.status.code(), Some(0), "activate exits 0:\n{stdout}\n{}", String::from_utf8_lossy(&out.stderr));
    assert!(stdout.contains("Logged in as alice@x.com"), "echoes the account:\n{stdout}");
    assert!(stdout.contains("credentials.toml"), "says where it saved:\n{stdout}");

    // Persistence: config points at the hub, the credential authenticates.
    let cfg = std::fs::read_to_string(cfg_dir.join("config.toml")).unwrap();
    assert!(cfg.contains(&url), "activate persists the server:\n{cfg}");
    let creds: toml::Value =
        toml::from_str(&std::fs::read_to_string(cfg_dir.join("credentials.toml")).unwrap()).unwrap();
    let token = creds["servers"][hub.addr.as_str()]["token"].as_str().expect("stored token");
    assert!(!stdout.contains(token), "the token is never printed");
    let r = reqwest::Client::new()
        .get(format!("{url}/api/sessions"))
        .bearer_auth(token)
        .send()
        .await
        .unwrap();
    assert!(r.status().is_success(), "stored token authenticates: {}", r.status());
}

/// `ccs logout`: revokes server-side (old token 401s) and removes the local
/// entry; a repeat logout is a friendly no-op (acceptance 5).
#[tokio::test]
async fn logout_revokes_and_forgets() {
    let hub = start_hub_multi().await;
    let (_alice, cookie) = hub.user_with_cookie("alice@x.com").await;
    let (xdg, cfg_dir) = fresh_xdg("out");
    let url = format!("http://{}", hub.addr);

    // Mint a client token over the raw wire (fast) and seed credentials.toml
    // the way activate writes it.
    let http = reqwest::Client::new();
    let code: serde_json::Value = http
        .post(format!("{url}/api/device/client/code"))
        .json(&serde_json::json!({ "label": "x" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(hub.approve(&cookie, code["user_code"].as_str().unwrap()).await, 200);
    let tok: serde_json::Value = http
        .post(format!("{url}/api/device/client/token"))
        .json(&serde_json::json!({ "device_code": code["device_code"] }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let token = tok["client_token"].as_str().unwrap().to_string();
    std::fs::create_dir_all(&cfg_dir).unwrap();
    std::fs::write(
        cfg_dir.join("credentials.toml"),
        format!("[servers.\"{}\"]\ntoken = \"{token}\"\n", hub.addr),
    )
    .unwrap();

    let out = run(&xdg, &["logout", "--server", &url]).await;
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert!(stdout.contains("Logged out of"), "{stdout}");
    assert!(!stdout.contains("couldn't confirm"), "server-side revocation confirmed:\n{stdout}");

    // Server side: the token is dead. Local side: the entry is gone.
    let r = http.get(format!("{url}/api/sessions")).bearer_auth(&token).send().await.unwrap();
    assert_eq!(r.status().as_u16(), 401, "old token 401s after logout");
    let creds = std::fs::read_to_string(cfg_dir.join("credentials.toml")).unwrap_or_default();
    assert!(!creds.contains(&token), "local entry removed:\n{creds}");

    // Logging out again is a calm no-op.
    let again = run(&xdg, &["logout", "--server", &url]).await;
    assert_eq!(again.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&again.stdout).contains("nothing to do"));
}

/// C4 through the binary: `ccs activate` against a single-tenant hub exits 2
/// with the static-token guidance.
#[tokio::test]
async fn activate_against_single_tenant_exits_2() {
    let hub = start_hub(Auth::new(None, Some("tok".into()), [1u8; 32]), &[]).await;
    let (xdg, _cfg_dir) = fresh_xdg("st");
    let out = run(&xdg, &["activate", "--server", &format!("http://{hub}")]).await;
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("static token"), "names the alternative:\n{stderr}");
}

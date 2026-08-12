//! Proposal 0078 Part D — the `state.toml` recents store, against a real file.
//!
//! Its own test binary (integration tests are separate processes) so setting
//! `XDG_CONFIG_HOME` for the whole process can't leak into the e2e suite. One
//! sequential test, deliberately: the claims are about a single shared file, and
//! splitting them into parallel `#[test]`s would make them race each other.

use std::path::PathBuf;

use cc_screen_tui::config::{
    forget_recent, host_key, load_state, promote_recent, recents_for, recents_for_key, state_path,
    update_recents, MAX_RECENTS,
};

fn cfg_dir() -> PathBuf {
    let xdg = std::env::temp_dir().join(format!("ccs-recents-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&xdg);
    std::fs::create_dir_all(&xdg).unwrap();
    std::env::set_var("XDG_CONFIG_HOME", &xdg);
    xdg.join("cc-screen-tui")
}

#[test]
fn state_toml_is_the_recents_store_and_config_toml_is_never_touched() {
    let dir = cfg_dir();
    let path = state_path().expect("a state path");
    assert_eq!(path.parent().unwrap(), dir);

    // A missing file is not an error, not a warning, and not a `.bad` sidecar —
    // this is machine-written state, unlike config.toml (0078 C1).
    assert!(load_state().hosts.is_empty());
    assert!(recents_for("http://hub-a:8840").is_empty());

    // Promote three sessions on one hub; most recent first.
    for name in ["alpha", "beta", "gamma"] {
        update_recents("hub-a:8840", |l| promote_recent(l, "pine", name));
    }
    let list = recents_for("http://hub-a:8840/");
    assert_eq!(
        list.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(),
        vec!["gamma", "beta", "alpha"]
    );
    // Keyed by (machine, name): the same name on another agent is another entry.
    update_recents("hub-a:8840", |l| promote_recent(l, "studio", "alpha"));
    let list = recents_for("hub-a:8840");
    assert_eq!(list.len(), 4);
    assert!(list[0].is("studio", "alpha"));

    // The whole point of the move: attaching a session doesn't rewrite the
    // user-editable config (the [0060] Part A clobber).
    assert!(path.exists(), "state.toml written");
    assert!(!dir.join("config.toml").exists(), "config.toml untouched by a recents write");

    // No temp file survives a write — the tmp+rename is pid-suffixed so two
    // writers can't share a partial file, and a reader never sees a truncation.
    let leftovers: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| n.contains(".tmp"))
        .collect();
    assert!(leftovers.is_empty(), "no state.toml.tmp* left behind: {leftovers:?}");

    // Two hubs' histories don't interleave.
    // `update_recents` takes the STORE KEY (what `App` holds as its state
    // scope), so a caller with a URL normalises it through `host_key` first.
    update_recents(&host_key("https://other.example/"), |l| promote_recent(l, "", "solo"));
    assert_eq!(recents_for("hub-a:8840").len(), 4);
    let other = recents_for("other.example");
    assert_eq!(other.len(), 1);
    assert!(other[0].is("", "solo"), "a direct agent's empty machine is a valid key");

    // Read-modify-write: a write from "another process" between our read and our
    // write is merged, not clobbered.
    let mut state = load_state();
    state
        .hosts
        .get_mut("hub-a:8840")
        .unwrap()
        .recents
        .insert(0, cc_screen_tui::config::RecentSession::new("pine", "from-other-ccs"));
    let body = toml::to_string_pretty(&state).unwrap();
    std::fs::write(&path, body).unwrap();
    update_recents("hub-a:8840", |l| promote_recent(l, "pine", "beta"));
    let names: Vec<String> =
        recents_for("hub-a:8840").into_iter().map(|r| r.name).collect();
    assert_eq!(names[0], "beta");
    assert!(names.contains(&"from-other-ccs".to_string()), "the other writer survives: {names:?}");

    // The cap evicts, and an explicit forget is the only other prune edge.
    for i in 0..MAX_RECENTS {
        update_recents("hub-a:8840", |l| promote_recent(l, "pine", &format!("s{i}")));
    }
    assert_eq!(recents_for_key("hub-a:8840").len(), MAX_RECENTS);
    assert!(update_recents("hub-a:8840", |l| forget_recent(l, "pine", "s0")).iter().all(|r| !r.is("pine", "s0")));

    // A corrupt file degrades to "no Recent section", silently.
    std::fs::write(&path, "hosts = [ this is not toml").unwrap();
    assert!(load_state().hosts.is_empty());
    assert!(recents_for("hub-a:8840").is_empty());

    // An unwritable state dir leaves the client working and quiet: the write
    // fails, the returned list is still correct in memory, nothing panics.
    std::fs::remove_file(&path).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o500)).unwrap();
        let out = update_recents("hub-a:8840", |l| promote_recent(l, "pine", "after-readonly"));
        assert_eq!(out.len(), 1);
        assert!(!path.exists());
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
}

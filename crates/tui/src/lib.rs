//! `ccs` — the cc-screen-rust terminal client, as a library.
//!
//! The binary (`src/main.rs`) is a thin shell over these modules; exposing them
//! as a library is what lets the e2e harness (`tests/e2e.rs`, `tests/pty_smoke.rs`)
//! build a real `App` and drive it against an in-process hub (proposal 0059 B2/B3).
//! A bin-only crate can't be reached from an integration test — hence the lib.

pub mod anchored_backend;
pub mod app;
pub mod cli;
pub mod client;
pub mod config;
pub mod input;
pub mod layout;
pub mod pane;
pub mod ready;
pub mod term;
pub mod ui;

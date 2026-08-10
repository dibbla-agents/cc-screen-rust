//! Transactional mail (proposal 0073) — multi-tenant only, and inert unless a
//! transport is configured. Invite delivery is best-effort *discovery* on top of
//! the invite row that already exists, never the grant ([0040] §8, [0056] C4): a
//! forwarded message still converts nothing, because accepting requires signing
//! in with the invited address.
//!
//! Off by default the way Stripe is off by default ([0058]): with neither
//! `CCHUB_SMTP_URL` nor `CCHUB_MAIL_DIR` set the transport is [`Transport::Off`],
//! [`Mailer::active`] is false, and every call site skips its spawn — a
//! self-hosted hub behaves byte-for-byte as it did before, copyable invite link
//! and all.
//!
//! Env (read once, in `main`, into the resolved [`Mailer`] on `HubState` — not
//! re-read per call the way `oauth`/`billing` do, because the test suite runs
//! many hubs in one process and a process-global mailer could not be both on and
//! off in one run):
//!
//! | key | meaning |
//! |---|---|
//! | `CCHUB_SMTP_URL` | `smtp://user:key@host:587` (or `smtps://…`). Unset ⇒ no mail. |
//! | `CCHUB_MAIL_DIR` | capture transport: write each message to a file instead of sending. Wins over `CCHUB_SMTP_URL` so a test box with stray SMTP env can never send real mail. |
//! | `CCHUB_MAIL_FROM` | envelope + `From`. Must be on a domain the relay is authorized for. |
//! | `CCHUB_MAIL_REPLY_TO` | `Reply-To`; falls back to `CCHUB_SUPPORT_EMAIL`. Not cosmetic — `ccscreen.dev` has no MX, so without it a reply vanishes. |
//! | `CCHUB_MAIL_FOOTER` | the company disclosure line (ABL 2005:551 kap. 28 §5). |
//! | `CCHUB_PUBLIC_URL` | **required**: mailing a relative or `localhost` invite link is worse than mailing nothing, so an unset base disables the mailer. |
//!
//! Bodies are `text/plain` only (A4): renders everywhere, cannot carry a
//! tracking pixel, and makes the "no marketing mail, ever" Non-Goal structural
//! rather than a promise. One message per invitation — no reminders, no
//! follow-ups, no unsubscribe (an invitation is not a subscription).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use lettre::message::{header::ContentType, Mailbox, Message};
use lettre::transport::smtp::AsyncSmtpTransport;
use lettre::{AsyncTransport, Tokio1Executor};

/// Ceiling on one delivery attempt. Nothing in `crates/hub` sets a client
/// timeout, so the in-repo idiom is an explicit `tokio::time::timeout`
/// (`bulk.rs`, `registry.rs`); a hung relay must not leak a task per invite.
const SEND_TIMEOUT: Duration = Duration::from_secs(20);

/// Longest error detail we ever log or persist. `billing.rs` truncates Stripe
/// bodies the same way so a credential-bearing error can't flood the journal.
const DETAIL_MAX: usize = 300;

/// Outcome of one delivery attempt. Mirrors `push::SendErr`'s split: a permanent
/// rejection is worth recording against the address (the UI must not offer a
/// Resend that cannot succeed); anything else is transient and worth a log line
/// plus a Resend affordance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MailOutcome {
    Sent,
    Rejected(String),
    Failed(String),
}

impl MailOutcome {
    /// What the `delivery` column stores (B2). Kept a `&'static str` so the
    /// persistence layer never has to know this module exists.
    pub fn as_str(&self) -> &'static str {
        match self {
            MailOutcome::Sent => "sent",
            MailOutcome::Rejected(_) => "rejected",
            MailOutcome::Failed(_) => "failed",
        }
    }

    /// The truncated diagnostic for the audit row / log line.
    pub fn detail(&self) -> String {
        match self {
            MailOutcome::Sent => "sent".to_string(),
            MailOutcome::Rejected(e) | MailOutcome::Failed(e) => {
                format!("{}: {}", self.as_str(), truncate(e))
            }
        }
    }
}

fn truncate(s: &str) -> String {
    s.chars().take(DETAIL_MAX).collect()
}

enum Transport {
    /// Unconfigured: `send` is never reached, because every call site checks
    /// [`Mailer::active`] first.
    Off,
    /// Capture: render the message to a file and report `Sent`. What every test
    /// harness uses (E1) — it is why this proposal adds no SMTP container to CI.
    Dir(PathBuf),
    Smtp(Box<AsyncSmtpTransport<Tokio1Executor>>),
}

/// Resolved mail config + the transport, built once at startup.
pub struct Mailer {
    transport: Transport,
    from: Option<Mailbox>,
    reply_to: Option<Mailbox>,
    /// Public origin the invite links were built against. Held so the footer can
    /// link the site, and because an unset one disables the mailer entirely.
    public_url: String,
    footer: String,
    /// Monotonic suffix so two captures in the same nanosecond can't collide.
    seq: AtomicU64,
}

impl Mailer {
    /// Build from env. `Transport::Off` (i.e. [`active`](Self::active) false)
    /// unless a transport **and** `CCHUB_PUBLIC_URL` are both configured.
    pub fn from_env() -> Self {
        let var = |k: &str| std::env::var(k).ok().map(|v| v.trim().to_string()).filter(|v| !v.is_empty());

        // Either key configures a mailer; a maildir wins so a test box with stray
        // SMTP env in its shell can never send real mail.
        let dir = var("CCHUB_MAIL_DIR");
        let url = var("CCHUB_SMTP_URL");
        if dir.is_none() && url.is_none() {
            return Self::disabled();
        }
        // Required, not optional: this is the one place the hub's three different
        // fallbacks for CCHUB_PUBLIC_URL turn dangerous. Mailing `/org-invite/<t>`
        // or a `localhost` link is worse than mailing nothing.
        let Some(public_url) = var("CCHUB_PUBLIC_URL").map(|v| v.trim_end_matches('/').to_string()) else {
            tracing::warn!("mailer: CCHUB_SMTP_URL/CCHUB_MAIL_DIR set but CCHUB_PUBLIC_URL is not — mail disabled (an invite link must be absolute)");
            return Self::disabled();
        };

        let transport = match dir {
            Some(d) => Transport::Dir(PathBuf::from(d)),
            None => match build_smtp(url.as_deref().unwrap_or_default()) {
                Ok(t) => Transport::Smtp(Box::new(t)),
                Err(e) => {
                    tracing::warn!("mailer: CCHUB_SMTP_URL is not a usable SMTP URL ({}) — mail disabled", truncate(&e.to_string()));
                    return Self::disabled();
                }
            },
        };

        let from = parse_mailbox(&var("CCHUB_MAIL_FROM").unwrap_or_else(|| DEFAULT_FROM.to_string()));
        if from.is_none() {
            tracing::warn!("mailer: CCHUB_MAIL_FROM is not a valid address — mail disabled");
            return Self::disabled();
        }
        // Fall back per *parse*, not per *presence*: a typo'd CCHUB_MAIL_REPLY_TO
        // used to yield `None` silently AND skip the CCHUB_SUPPORT_EMAIL fallback
        // (the `.or_else` had already fired on the `Some`), so mail went out with
        // no reply path at all — which the module doc calls not-cosmetic, since
        // ccscreen.dev has no MX. Unlike `from`, an unusable Reply-To does not
        // disable the mailer: an invitation with no reply path still beats no
        // invitation. It does warn.
        let support = var("CCHUB_SUPPORT_EMAIL");
        let reply_to = var("CCHUB_MAIL_REPLY_TO")
            .and_then(|s| parse_mailbox(&s).or_else(|| {
                tracing::warn!("mailer: CCHUB_MAIL_REPLY_TO is not a valid address — falling back to CCHUB_SUPPORT_EMAIL");
                None
            }))
            .or_else(|| support.as_deref().and_then(parse_mailbox));
        if reply_to.is_none() {
            tracing::warn!("mailer: no usable Reply-To (CCHUB_MAIL_REPLY_TO / CCHUB_SUPPORT_EMAIL) — replies to invitations will be discarded");
        }

        Mailer {
            transport,
            from,
            reply_to,
            footer: var("CCHUB_MAIL_FOOTER").unwrap_or_else(|| default_footer(support.as_deref())),
            public_url,
            seq: AtomicU64::new(0),
        }
    }

    /// A mailer that sends nothing — the default, and what the four non-`main`
    /// `HubState` construction sites take (the split `Summarizer` already has).
    pub fn disabled() -> Self {
        Mailer {
            transport: Transport::Off,
            from: None,
            reply_to: None,
            public_url: String::new(),
            footer: String::new(),
            seq: AtomicU64::new(0),
        }
    }

    /// A capture mailer for harnesses: every message is written to `dir` as an
    /// `.eml` file (full RFC 5322 bytes, headers included) and reported `Sent`.
    pub fn capturing(dir: PathBuf) -> Self {
        Mailer {
            transport: Transport::Dir(dir),
            from: parse_mailbox(DEFAULT_FROM),
            reply_to: None,
            public_url: "https://hub.test".to_string(),
            footer: default_footer(None),
            seq: AtomicU64::new(0),
        }
    }

    /// Whether this hub can emit mail. Feeds `/api/me`'s `mail` flag (D1) and
    /// guards every send site, so an unconfigured hub spawns no task at all.
    pub fn active(&self) -> bool {
        !matches!(self.transport, Transport::Off)
    }

    /// The public origin links in a body are built against (empty when off).
    pub fn public_url(&self) -> &str {
        &self.public_url
    }

    /// Deliver one message. Never panics and never propagates: the caller
    /// records the outcome and moves on. There are **no retries** — `crates/push`
    /// has none either, a `Failed` invite is visible in the outbox and
    /// re-sendable by a human (D2), and a retry queue is a queue the hub has no
    /// abstraction for.
    pub async fn send(&self, to: &str, subject: &str, text: &str) -> MailOutcome {
        let (Some(from), true) = (self.from.clone(), self.active()) else {
            return MailOutcome::Failed("mailer not configured".into());
        };
        let to: Mailbox = match to.parse() {
            Ok(m) => m,
            // A permanent property of the address, not a transient failure: no
            // Resend can fix it.
            Err(e) => return MailOutcome::Rejected(format!("invalid recipient: {e}")),
        };
        // `message_id(None)` makes lettre generate one from the `hostname`
        // feature. `build()` only auto-inserts `Date`, so without this every
        // message ships without a Message-ID — a real spam-filter signal, and a
        // cheap one to give a brand-new sending domain (A4's deliverability
        // posture is the whole argument for the plain-text body and the footer).
        let mut builder = Message::builder().from(from).to(to).subject(subject).message_id(None);
        if let Some(rt) = self.reply_to.clone() {
            builder = builder.reply_to(rt);
        }
        // Headers are only ever built through lettre's typed builder, which
        // parses addresses and RFC 2047-encodes the subject rather than
        // concatenating attacker-supplied bytes into a header (C3).
        let msg = match builder.header(ContentType::TEXT_PLAIN).body(self.render(text)) {
            Ok(m) => m,
            Err(e) => return MailOutcome::Rejected(format!("message rejected: {e}")),
        };

        match &self.transport {
            Transport::Off => MailOutcome::Failed("mailer not configured".into()),
            Transport::Dir(dir) => match self.capture(dir, &msg) {
                Ok(()) => MailOutcome::Sent,
                Err(e) => {
                    tracing::warn!("mailer: capture write failed: {}", truncate(&e.to_string()));
                    MailOutcome::Failed(e.to_string())
                }
            },
            Transport::Smtp(t) => match tokio::time::timeout(SEND_TIMEOUT, t.send(msg)).await {
                Ok(Ok(_)) => MailOutcome::Sent,
                Ok(Err(e)) => {
                    // lettre exposes the relay's 5xx/4xx split; a permanent
                    // refusal is worth recording against the address, everything
                    // else is transient (push's SendErr::Gone / ::Other split).
                    let detail = truncate(&e.to_string());
                    if e.is_permanent() {
                        tracing::warn!("mailer: relay permanently rejected the message: {detail}");
                        MailOutcome::Rejected(detail)
                    } else {
                        tracing::warn!("mailer: send failed: {detail}");
                        MailOutcome::Failed(detail)
                    }
                }
                Err(_) => {
                    tracing::warn!("mailer: send timed out after {}s", SEND_TIMEOUT.as_secs());
                    MailOutcome::Failed(format!("timed out after {}s", SEND_TIMEOUT.as_secs()))
                }
            },
        }
    }

    /// Append the company disclosure to a body. Kept out of the templates so
    /// every message carries it and no template can forget.
    fn render(&self, text: &str) -> String {
        if self.footer.is_empty() {
            return text.to_string();
        }
        format!("{text}\n--\n{}\n", self.footer)
    }

    fn capture(&self, dir: &Path, msg: &Message) -> std::io::Result<()> {
        std::fs::create_dir_all(dir)?;
        let n = self.seq.fetch_add(1, Ordering::Relaxed);
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default();
        std::fs::write(dir.join(format!("{stamp:039}-{n:04}.eml")), msg.formatted())
    }
}

/// The default sender. Overridable, because a self-hoster's relay will not be
/// authorized for `ccscreen.dev`.
const DEFAULT_FROM: &str = "cc-screen <invites@ccscreen.dev>";

/// The disclosure an aktiebolag owes under ABL 2005:551 kap. 28 §5 and lagen om
/// elektronisk handel 2002:562 §8-10 — and, independently, one of the cheapest
/// legitimacy signals a spam filter reads. Override with `CCHUB_MAIL_FOOTER` to
/// add org.nr/säte or to name a different entity on a self-hosted hub.
fn default_footer(support: Option<&str>) -> String {
    match support {
        Some(s) => format!("Techier AI Sthlm AB · Stockholm, Sweden · {s}"),
        None => "Techier AI Sthlm AB · Stockholm, Sweden".to_string(),
    }
}

fn parse_mailbox(s: &str) -> Option<Mailbox> {
    s.parse().ok()
}

/// Build the SMTP transport from `CCHUB_SMTP_URL`.
///
/// One deliberate deviation from `AsyncSmtpTransport::from_url`: lettre reads a
/// bare `smtp://` (no `tls=` parameter) as **plaintext, no STARTTLS**, which
/// would AUTH a relay credential in the clear against the very URL shape [0056]
/// wrote down (`smtp://user:key@smtp-relay.brevo.com:587`). So a bare `smtp://`
/// is upgraded here: `tls=required` for a remote host, `tls=opportunistic` for
/// loopback (a dev relay on 127.0.0.1 usually offers no TLS at all). An explicit
/// `?tls=` or an `smtps://` scheme is honored verbatim.
fn build_smtp(url: &str) -> anyhow::Result<AsyncSmtpTransport<Tokio1Executor>> {
    let effective = upgrade_smtp_url(url);
    Ok(AsyncSmtpTransport::<Tokio1Executor>::from_url(&effective)?.build())
}

fn upgrade_smtp_url(url: &str) -> String {
    let trimmed = url.trim();
    if !trimmed.starts_with("smtp://") || trimmed.contains("tls=") {
        return trimmed.to_string();
    }
    let host_part = trimmed.trim_start_matches("smtp://").rsplit('@').next().unwrap_or_default();
    let host = host_part.split(['/', ':', '?']).next().unwrap_or_default();
    let loopback = matches!(host, "localhost" | "127.0.0.1" | "::1" | "[::1]");
    let mode = if loopback { "opportunistic" } else { "required" };
    let sep = if trimmed.contains('?') { '&' } else { '?' };
    format!("{trimmed}{sep}tls={mode}")
}

// ── message templates (A4) ───────────────────────────────────────────────────
//
// Plain text, one template per message, values interpolated into the BODY only
// (never a header) except the org name in the subject, which is safe because C3
// rejects control characters at the source and `.subject()` RFC 2047-encodes.
// Each body says *why it exists and who supplied the address*: the recipient is
// not yet anyone's user, so this mail is the GDPR art. 14 notice, and it is also
// the honest answer to "why am I getting this" that keeps a transactional
// invitation out of a spam report.

/// The team-invite message. The consent sentence is [0063]'s [`CONSENT_COPY`]
/// **verbatim** — it is part of the consent model, not decoration.
///
/// [`CONSENT_COPY`]: crate::org::CONSENT_COPY
pub fn org_invite_message(org_name: &str, inviter: &str, invite_url: &str) -> (String, String) {
    let subject = format!("{inviter} invited you to the team \"{org_name}\" on cc-screen");
    let body = format!(
        "{inviter} invited you to join the team \"{org_name}\" on cc-screen.\n\
         \n\
         Accept the invitation:\n\
         {invite_url}\n\
         \n\
         {consent}\n\
         \n\
         The link expires in 14 days. It identifies the invitation but grants \
         nothing on its own: you can only accept it while signed in with this \
         email address, so forwarding it gives no one access.\n\
         \n\
         You are receiving this because {inviter} entered this address when \
         inviting you to their team on cc-screen. If you were not expecting it, \
         you can ignore this message — nothing happens until you accept.\n",
        consent = crate::org::CONSENT_COPY,
    );
    (subject, body)
}

/// The share-invite message. `what` is the human description of the shared
/// resource ("machine pine" / "session api on pine") — never whether the address
/// has an account.
pub fn share_invite_message(inviter: &str, what: &str, invite_url: &str) -> (String, String) {
    let subject = format!("{inviter} shared {what} with you on cc-screen");
    let body = format!(
        "{inviter} shared {what} with you on cc-screen.\n\
         \n\
         Accept the invitation:\n\
         {invite_url}\n\
         \n\
         The link expires in 14 days. It identifies the invitation but grants \
         nothing on its own: you can only accept it while signed in with this \
         email address, so forwarding it gives no one access.\n\
         \n\
         You are receiving this because {inviter} entered this address when \
         sharing with you on cc-screen. If you were not expecting it, you can \
         ignore this message — nothing happens until you accept.\n"
    );
    (subject, body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unconfigured_is_off() {
        let m = Mailer::disabled();
        assert!(!m.active());
    }

    #[tokio::test]
    async fn disabled_send_never_writes_and_reports_failure() {
        let m = Mailer::disabled();
        assert!(matches!(m.send("a@b.com", "s", "t").await, MailOutcome::Failed(_)));
    }

    #[tokio::test]
    async fn capturing_writes_one_file_per_message() {
        let dir = std::env::temp_dir().join(format!("ccr-mailer-{}-{:?}", std::process::id(), std::thread::current().id()));
        let _ = std::fs::remove_dir_all(&dir);
        let m = Mailer::capturing(dir.clone());
        assert!(m.active());
        assert_eq!(m.send("who@example.com", "Subject here", "Body here").await, MailOutcome::Sent);
        assert_eq!(m.send("who@example.com", "Subject here", "Body here").await, MailOutcome::Sent);
        let files: Vec<_> = std::fs::read_dir(&dir).unwrap().filter_map(|e| e.ok()).collect();
        assert_eq!(files.len(), 2, "one file per message");
        let raw = std::fs::read_to_string(files[0].path()).unwrap();
        assert!(raw.contains("To: who@example.com"), "{raw}");
        assert!(raw.contains("Body here"), "{raw}");
        assert!(raw.contains("Techier AI Sthlm AB"), "footer on every message: {raw}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// No interpolated value may become a second header, whatever it carries.
    ///
    /// This covers the two values that reach the **subject** with no validator in
    /// front of them — the share path's `what` (a session name, which is not even
    /// checked for existence) and `inviter` (the actor's own signup address) —
    /// as well as the org name, which does have one. `lettre`'s typed builder is
    /// the structural defense here; C3's validators are the first line, and the
    /// point of this test is that the second line holds on its own.
    ///
    /// Deliberately *not* written as "if it sent, then assert": that shape passes
    /// vacuously the moment `send` starts returning `Rejected` for an unrelated
    /// reason. Either outcome is acceptable for security, so the outcome is
    /// asserted separately from the headers.
    #[tokio::test]
    async fn a_hostile_value_cannot_inject_a_header() {
        // Every byte class that could plausibly terminate a header: CRLF, bare
        // CR, bare LF, NUL, DEL, and the Unicode line/paragraph separators.
        let payloads = [
            "Acme\r\nBcc: attacker@evil.com",
            "Acme\rBcc: attacker@evil.com",
            "Acme\nBcc: attacker@evil.com",
            "Acme\u{0}Bcc: attacker@evil.com",
            "Acme\u{7f}Bcc: attacker@evil.com",
            "Acme\u{2028}Bcc: attacker@evil.com",
            "Acme\u{2029}Bcc: attacker@evil.com",
            // CRLF at a word boundary — lettre encodes per whitespace-delimited
            // word, so this is the shape most likely to survive unencoded.
            "Acme\r\n Bcc: attacker@evil.com",
        ];
        for (i, p) in payloads.iter().enumerate() {
            let dir = std::env::temp_dir()
                .join(format!("ccr-mailer-inj-{}-{i}-{:?}", std::process::id(), std::thread::current().id()));
            let _ = std::fs::remove_dir_all(&dir);
            let m = Mailer::capturing(dir.clone());
            // All three interpolation sites, including the two with no validator.
            for (subject, body) in [
                org_invite_message(p, "a@b.com", "https://h/x"),
                share_invite_message(p, "machine pine", "https://h/x"),
                share_invite_message("a@b.com", &format!("session {p} on pine"), "https://h/x"),
            ] {
                let out = m.send("victim@example.com", &subject, &body).await;
                assert!(
                    matches!(out, MailOutcome::Sent | MailOutcome::Rejected(_)),
                    "unexpected transport failure for {p:?}: {out:?}"
                );
            }
            for f in std::fs::read_dir(&dir).into_iter().flatten().filter_map(|e| e.ok()) {
                let raw = std::fs::read_to_string(f.path()).unwrap();
                // Only the *header block* matters — the same bytes in the body are
                // inert text that is never re-parsed.
                let headers = raw.split("\r\n\r\n").next().unwrap_or_default();
                let lower = headers.to_lowercase();
                assert!(!lower.contains("\r\nbcc:") && !lower.starts_with("bcc:"), "injected Bcc for {p:?}: {raw}");
                assert!(!lower.contains("\r\ncc:") && !lower.starts_with("cc:"), "injected Cc for {p:?}: {raw}");
                assert_eq!(
                    lower.matches("\r\nto:").count() + lower.starts_with("to:") as usize,
                    1,
                    "recipient count moved for {p:?}: {raw}"
                );
                // A continuation line (leading space/tab) is more of the previous
                // header and is fine; what must never happen is the injected text
                // starting a line at column 0.
                for line in headers.split("\r\n") {
                    assert!(
                        !line.to_lowercase().starts_with("bcc:") || line.starts_with([' ', '\t']),
                        "injected header at column 0 for {p:?}: {raw}"
                    );
                }
            }
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    #[test]
    fn bare_smtp_url_is_upgraded_to_starttls() {
        assert_eq!(
            upgrade_smtp_url("smtp://u:p@smtp-relay.brevo.com:587"),
            "smtp://u:p@smtp-relay.brevo.com:587?tls=required"
        );
        // Loopback dev relays usually offer no TLS at all.
        assert_eq!(upgrade_smtp_url("smtp://127.0.0.1:1025"), "smtp://127.0.0.1:1025?tls=opportunistic");
        // Explicit intent is honored verbatim.
        assert_eq!(upgrade_smtp_url("smtp://h:25?tls=opportunistic"), "smtp://h:25?tls=opportunistic");
        assert_eq!(upgrade_smtp_url("smtps://u:p@h:465"), "smtps://u:p@h:465");
    }

    #[test]
    fn detail_is_truncated_and_maps_to_a_column_value() {
        assert_eq!(MailOutcome::Sent.as_str(), "sent");
        assert_eq!(MailOutcome::Rejected("x".into()).as_str(), "rejected");
        assert_eq!(MailOutcome::Failed("x".into()).as_str(), "failed");
        let long = MailOutcome::Failed("e".repeat(5_000));
        assert!(long.detail().len() <= DETAIL_MAX + 16);
    }

    #[test]
    fn both_templates_carry_the_url_and_the_why() {
        let (s, b) = org_invite_message("Acme", "boss@acme.com", "https://app/org-invite/tok");
        assert!(s.contains("Acme") && s.contains("boss@acme.com"));
        assert!(b.contains("https://app/org-invite/tok"));
        assert!(b.contains(crate::org::CONSENT_COPY), "0063 consent copy verbatim");
        assert!(b.contains("You are receiving this because"));
        let (s, b) = share_invite_message("boss@acme.com", "machine pine", "https://app/invite/tok");
        assert!(s.contains("machine pine"));
        assert!(b.contains("https://app/invite/tok") && b.contains("You are receiving this because"));
    }
}

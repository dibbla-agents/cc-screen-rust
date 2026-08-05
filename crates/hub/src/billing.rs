//! Stripe Managed Payments (proposal 0058 Part B), compiled only under
//! `multi-tenant`. Three routes — `checkout`, `portal`, `webhook` — plus a nightly
//! `reconcile`, and a ~100-line raw-reqwest Stripe client (no SDK). Everything is
//! **env-gated**: with no `STRIPE_*` env [`is_configured`] is false, the routes are
//! never registered (`lib.rs`), `/api/me` reports `billing:false`, and a
//! self-hosted hub behaves exactly as before — manual `user plan` is the whole
//! billing story.
//!
//! Design invariants (proposal 0058 B1–B5):
//! - Billing truth lives in Stripe; the hub reads local `users` columns on the
//!   entitlement hot path and never calls Stripe from a request that gates access.
//! - The webhook verifies the `Stripe-Signature` HMAC (via `cc-screen-auth`) and
//!   does ONE SQLite transaction (idempotency insert + state change) so a crash
//!   rolls back and Stripe's retry reprocesses.
//! - Every subscription-shaped event triggers a re-fetch and applies *current API
//!   truth*, so out-of-order delivery converges.

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::json;

use crate::db::{Store, SubApply};
use crate::state::HubState;

const STRIPE_API_BASE: &str = "https://api.stripe.com";
/// Pinned API version on every outbound call. Managed Payments requires
/// `2025-03-31.basil` or later, and the parsing below follows basil's field
/// moves (`current_period_end` lives on subscription items, the invoice's
/// subscription id under `parent.subscription_details`) — pinning keeps the
/// response shape independent of the account's default version.
const STRIPE_API_VERSION: &str = "2025-03-31.basil";
/// Stripe's recommended signature tolerance window (proposal 0058 B3).
const SIG_TOLERANCE_SECS: u64 = 300;
/// Founder-offer deadline default: 2026-10-01T00:00:00Z (proposal 0058 C1/D1),
/// overridable via `STRIPE_FOUNDER_DEADLINE` (unix seconds).
const DEFAULT_FOUNDER_DEADLINE: u64 = 1_790_812_800;

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Optional billing config, read module-locally from env (the `oauth.rs` precedent,
/// NOT a `HubConfig` extension). The secret key, webhook secret, and the two Pro
/// price ids are required; the founder price is optional (it's created later in the
/// rollout — D1) and simply doesn't apply when unset.
pub struct BillingConfig {
    secret_key: String,
    webhook_secret: String,
    price_pro_monthly: String,
    price_pro_annual: String,
    price_pro_founder: Option<String>,
    founder_deadline: u64,
    public_url: String,
    /// Stripe Managed Payments (merchant of record). Opt-in via
    /// `STRIPE_MANAGED_PAYMENTS=1`: ccscreen.dev runs with it on; a self-hosted
    /// hub with a plain (non-MoR) Stripe account leaves it off — Stripe rejects
    /// the parameter on accounts without Managed Payments activated.
    managed_payments: bool,
}

impl BillingConfig {
    pub fn from_env() -> Option<Self> {
        let var = |k: &str| std::env::var(k).ok().map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
        let public_url = var("CCHUB_PUBLIC_URL")
            .unwrap_or_else(|| "http://localhost:8840".to_string())
            .trim_end_matches('/')
            .to_string();
        let founder_deadline = var("STRIPE_FOUNDER_DEADLINE")
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(DEFAULT_FOUNDER_DEADLINE);
        Some(BillingConfig {
            secret_key: var("STRIPE_SECRET_KEY")?,
            webhook_secret: var("STRIPE_WEBHOOK_SECRET")?,
            price_pro_monthly: var("STRIPE_PRICE_PRO_MONTHLY")?,
            price_pro_annual: var("STRIPE_PRICE_PRO_ANNUAL")?,
            price_pro_founder: var("STRIPE_PRICE_PRO_FOUNDER"),
            founder_deadline,
            public_url,
            managed_payments: matches!(
                var("STRIPE_MANAGED_PAYMENTS").as_deref(),
                Some("1") | Some("true") | Some("yes")
            ),
        })
    }

    /// Does a price id belong to one of our configured Pro prices? An unrecognized
    /// price on a subscription is logged and skipped (reconcile keeps flagging it).
    fn recognizes_price(&self, price_id: &str) -> bool {
        price_id == self.price_pro_monthly
            || price_id == self.price_pro_annual
            || self.price_pro_founder.as_deref() == Some(price_id)
    }
}

/// Whether Stripe billing is configured (all required env present). Gates route
/// registration in `lib.rs` and the `billing` flag in `/api/me`.
pub fn is_configured() -> bool {
    BillingConfig::from_env().is_some()
}

// ── the tiny Stripe client (four calls, raw reqwest + serde) ──────────────────

#[derive(Deserialize)]
struct Subscription {
    id: String,
    status: String,
    #[serde(default)]
    current_period_end: Option<i64>,
    customer: String,
    #[serde(default)]
    items: SubItems,
}
#[derive(Deserialize, Default)]
struct SubItems {
    #[serde(default)]
    data: Vec<SubItem>,
}
#[derive(Deserialize)]
struct SubItem {
    price: SubPrice,
    /// basil moved `current_period_end` from the subscription onto its items.
    #[serde(default)]
    current_period_end: Option<i64>,
}
#[derive(Deserialize)]
struct SubPrice {
    id: String,
}

impl Subscription {
    /// The price id on the (single) subscription item — `items.data[0].price.id`.
    fn price_id(&self) -> Option<&str> {
        self.items.data.first().map(|i| i.price.id.as_str())
    }

    /// Period end: the top-level field pre-basil, `items.data[0]` from basil on.
    fn period_end(&self) -> Option<i64> {
        self.current_period_end
            .or_else(|| self.items.data.first().and_then(|i| i.current_period_end))
    }
}

#[derive(Deserialize)]
struct UrlResp {
    url: String,
}
#[derive(Deserialize)]
struct SubList {
    #[serde(default)]
    data: Vec<Subscription>,
    #[serde(default)]
    has_more: bool,
}

/// `POST /v1/checkout/sessions` — returns the hosted-checkout URL to redirect to.
async fn create_checkout_session(
    cfg: &BillingConfig,
    price_id: &str,
    user_id: &str,
    customer: Option<&str>,
) -> anyhow::Result<String> {
    let mut form: Vec<(String, String)> = vec![
        ("mode".into(), "subscription".into()),
        ("client_reference_id".into(), user_id.into()),
        ("line_items[0][price]".into(), price_id.into()),
        ("line_items[0][quantity]".into(), "1".into()),
        ("success_url".into(), format!("{}/billing/success", cfg.public_url)),
        ("cancel_url".into(), format!("{}/billing/cancel", cfg.public_url)),
    ];
    if cfg.managed_payments {
        form.push(("managed_payments[enabled]".into(), "true".into()));
    }
    if let Some(c) = customer {
        form.push(("customer".into(), c.into()));
    }
    let resp: UrlResp = stripe_post(cfg, "/v1/checkout/sessions", &form).await?;
    Ok(resp.url)
}

/// `POST /v1/billing_portal/sessions` — returns the customer-portal URL.
async fn create_portal_session(cfg: &BillingConfig, customer_id: &str) -> anyhow::Result<String> {
    let form = vec![
        ("customer".to_string(), customer_id.to_string()),
        ("return_url".to_string(), format!("{}/", cfg.public_url)),
    ];
    let resp: UrlResp = stripe_post(cfg, "/v1/billing_portal/sessions", &form).await?;
    Ok(resp.url)
}

/// `GET /v1/subscriptions/{id}` — `Ok(None)` if Stripe 404s (the subscription is
/// gone), `Err` on any other failure (transient — never downgrade on it).
async fn get_subscription(cfg: &BillingConfig, sub_id: &str) -> anyhow::Result<Option<Subscription>> {
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{STRIPE_API_BASE}/v1/subscriptions/{sub_id}"))
        .basic_auth(&cfg.secret_key, Some(""))
        .header("Stripe-Version", STRIPE_API_VERSION)
        .send()
        .await?;
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    anyhow::ensure!(resp.status().is_success(), "stripe GET subscription {}: {}", sub_id, resp.status());
    Ok(Some(resp.json().await?))
}

/// `GET /v1/subscriptions?status=all` — one page; `starting_after` pages forward.
async fn list_subscriptions(cfg: &BillingConfig, starting_after: Option<&str>) -> anyhow::Result<SubList> {
    let client = reqwest::Client::new();
    let mut req = client
        .get(format!("{STRIPE_API_BASE}/v1/subscriptions"))
        .basic_auth(&cfg.secret_key, Some(""))
        .header("Stripe-Version", STRIPE_API_VERSION)
        .query(&[("status", "all"), ("limit", "100")]);
    if let Some(after) = starting_after {
        req = req.query(&[("starting_after", after)]);
    }
    let resp = req.send().await?;
    anyhow::ensure!(resp.status().is_success(), "stripe LIST subscriptions: {}", resp.status());
    Ok(resp.json().await?)
}

/// Shared form-POST helper (secret key as basic-auth username, per Stripe).
async fn stripe_post<T: for<'de> Deserialize<'de>>(
    cfg: &BillingConfig,
    path: &str,
    form: &[(String, String)],
) -> anyhow::Result<T> {
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{STRIPE_API_BASE}{path}"))
        .basic_auth(&cfg.secret_key, Some(""))
        .header("Stripe-Version", STRIPE_API_VERSION)
        .form(form)
        .send()
        .await?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("stripe POST {path}: {status}: {}", body.chars().take(300).collect::<String>());
    }
    Ok(resp.json().await?)
}

// ── pure decision helpers (unit-tested; no I/O) ───────────────────────────────

/// Resolve a client-supplied price *choice* to a configured price id, applying the
/// server-side founder gate (proposal 0058 D1): a `beta` user before the deadline
/// upgrading monthly gets the founder price when one is configured. The client can
/// never post a raw `price_…`. `None` = unknown choice → 400.
fn resolve_checkout_price(plan: &str, choice: &str, now: u64, cfg: &BillingConfig) -> Option<String> {
    let founder_open = plan == "beta" && now < cfg.founder_deadline;
    match choice {
        "pro-monthly" => {
            if founder_open {
                if let Some(f) = cfg.price_pro_founder.as_deref() {
                    return Some(f.to_string());
                }
            }
            Some(cfg.price_pro_monthly.clone())
        }
        "pro-annual" => Some(cfg.price_pro_annual.clone()),
        _ => None,
    }
}

/// Map a Stripe subscription status to our intent: `(plan, plan_status)` where a
/// `plan` of `Some("pro")` keeps the paid plan and `None` is the terminal/cancel
/// path. `None` return = a status we don't act on (e.g. `incomplete`).
fn plan_intent(status: &str) -> Option<(Option<&'static str>, &'static str)> {
    match status {
        "active" | "trialing" => Some((Some("pro"), "active")),
        // Grace: payment failed / retrying — keep full Pro entitlements.
        "past_due" | "unpaid" => Some((Some("pro"), "past_due")),
        // Terminal.
        "canceled" | "incomplete_expired" => Some((None, "canceled")),
        _ => None,
    }
}

/// Build the [`SubApply`] for a re-fetched subscription (proposal 0058 B3). `None`
/// skips (an unrecognized price on an otherwise-paid sub, or a no-op status) —
/// reconcile keeps such a subscription flagged.
fn sub_to_apply(user_id: &str, sub: &Subscription, cfg: &BillingConfig) -> Option<SubApply> {
    let (plan, status) = plan_intent(&sub.status)?;
    if plan.is_some() {
        // A paid state must carry one of our known prices, or we don't grant Pro.
        match sub.price_id() {
            Some(p) if cfg.recognizes_price(p) => {}
            other => {
                tracing::warn!("billing: unrecognized price {:?} on {} — skipping", other, sub.id);
                return None;
            }
        }
    }
    Some(SubApply {
        user_id: user_id.to_string(),
        plan: plan.map(|s| s.to_string()),
        status: status.to_string(),
        // Paid → store the sub id; terminal → clear it.
        subscription_id: plan.is_some().then(|| sub.id.clone()),
        // Always keep the customer id fresh (kept across cancel for resubscribe).
        customer_id: Some(sub.customer.clone()),
        period_end: sub.period_end(),
    })
}

/// The reconcile decision (proposal 0058 B5): what to write for `row` given the
/// current Stripe subscription (`None` = the subscription is gone at Stripe).
/// Returns `None` when local state already matches — so reconcile makes zero
/// writes in steady state (acceptance 12).
fn reconcile_apply(row: &crate::db::BillingRow, sub: Option<&Subscription>, cfg: &BillingConfig) -> Option<SubApply> {
    match sub {
        Some(sub) => {
            let (plan, status) = plan_intent(&sub.status)?;
            if plan.is_some() {
                // Already the right paid state? No write.
                if row.plan == "pro" && row.plan_status.as_deref() == Some(status) {
                    return None;
                }
                sub_to_apply(&row.user_id, sub, cfg)
            } else {
                // Terminal at Stripe; already canceled locally? No write.
                if row.plan_status.as_deref() == Some("canceled") {
                    return None;
                }
                sub_to_apply(&row.user_id, sub, cfg)
            }
        }
        // Subscription gone at Stripe but we still think it's live → downgrade.
        None => {
            if matches!(row.plan_status.as_deref(), Some("active") | Some("past_due")) {
                Some(SubApply {
                    user_id: row.user_id.clone(),
                    plan: None,
                    status: "canceled".into(),
                    subscription_id: None,
                    customer_id: row.customer_id.clone(),
                    period_end: None,
                })
            } else {
                None
            }
        }
    }
}

// ── HTTP handlers ─────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct CheckoutBody {
    /// `"pro-monthly"` or `"pro-annual"` — resolved to a price id server-side.
    price: String,
}

/// `POST /api/billing/checkout` (cookie-authed) — create a Stripe Checkout session
/// and return `{"url":…}` for the PWA to navigate to.
pub async fn checkout(State(hub): State<HubState>, headers: HeaderMap, Json(body): Json<CheckoutBody>) -> Response {
    let Some(cfg) = BillingConfig::from_env() else {
        return (StatusCode::NOT_FOUND, "billing not configured").into_response();
    };
    let Some(user) = hub.client_auth.user_from_cookie(&headers) else {
        return (StatusCode::UNAUTHORIZED, "login required").into_response();
    };
    let plan = hub.limits_for(&user).await.plan;
    let Some(price_id) = resolve_checkout_price(&plan, &body.price, now_secs(), &cfg) else {
        return (StatusCode::BAD_REQUEST, "unknown price").into_response();
    };
    let Some(store) = hub.store() else {
        return (StatusCode::NOT_FOUND, "billing not configured").into_response();
    };
    // Reuse a known Stripe customer so a resubscribe attaches to the same record.
    let (customer_id, _sub) = store.billing_ids(&user).await;
    match create_checkout_session(&cfg, &price_id, &user, customer_id.as_deref()).await {
        Ok(url) => (StatusCode::OK, Json(json!({ "url": url }))).into_response(),
        Err(e) => {
            tracing::warn!("billing: checkout create failed: {e}");
            (StatusCode::BAD_GATEWAY, "payment provider error").into_response()
        }
    }
}

/// `POST /api/billing/portal` (cookie-authed) — open the Stripe customer portal.
/// 409 when the user has no stored customer id (they've never subscribed).
pub async fn portal(State(hub): State<HubState>, headers: HeaderMap) -> Response {
    let Some(cfg) = BillingConfig::from_env() else {
        return (StatusCode::NOT_FOUND, "billing not configured").into_response();
    };
    let Some(user) = hub.client_auth.user_from_cookie(&headers) else {
        return (StatusCode::UNAUTHORIZED, "login required").into_response();
    };
    let Some(store) = hub.store() else {
        return (StatusCode::NOT_FOUND, "billing not configured").into_response();
    };
    let (customer_id, _sub) = store.billing_ids(&user).await;
    let Some(customer_id) = customer_id else {
        return (StatusCode::CONFLICT, "no billing account — subscribe first").into_response();
    };
    match create_portal_session(&cfg, &customer_id).await {
        Ok(url) => (StatusCode::OK, Json(json!({ "url": url }))).into_response(),
        Err(e) => {
            tracing::warn!("billing: portal create failed: {e}");
            (StatusCode::BAD_GATEWAY, "payment provider error").into_response()
        }
    }
}

// Minimal shapes for the event objects we read.
#[derive(Deserialize)]
struct Event {
    id: String,
    #[serde(rename = "type")]
    kind: String,
    data: EventData,
}
#[derive(Deserialize)]
struct EventData {
    object: serde_json::Value,
}
#[derive(Deserialize)]
struct CheckoutSessionObj {
    #[serde(default)]
    client_reference_id: Option<String>,
    #[serde(default)]
    subscription: Option<String>,
}
#[derive(Deserialize)]
struct SubObj {
    id: String,
}
#[derive(Deserialize)]
struct InvoiceObj {
    /// Pre-basil shape: the subscription id sits directly on the invoice.
    #[serde(default)]
    subscription: Option<String>,
    /// basil shape: `parent.subscription_details.subscription`. The webhook
    /// payload's shape follows the *endpoint's* pinned API version, so both are
    /// accepted regardless of how the dashboard endpoint was created.
    #[serde(default)]
    parent: Option<InvoiceParent>,
}
#[derive(Deserialize)]
struct InvoiceParent {
    #[serde(default)]
    subscription_details: Option<InvoiceSubDetails>,
}
#[derive(Deserialize)]
struct InvoiceSubDetails {
    #[serde(default)]
    subscription: Option<String>,
}

impl InvoiceObj {
    fn subscription_id(self) -> Option<String> {
        self.subscription.or_else(|| {
            self.parent.and_then(|p| p.subscription_details).and_then(|d| d.subscription)
        })
    }
}

/// `POST /api/billing/webhook` — Stripe → hub. Verifies the signature, then does
/// one idempotent transaction. Exempt from the cookie gate (the HMAC is its auth);
/// see `handlers::require_client_auth`.
pub async fn webhook(State(hub): State<HubState>, headers: HeaderMap, body: Bytes) -> Response {
    let Some(cfg) = BillingConfig::from_env() else {
        return (StatusCode::NOT_FOUND, "billing not configured").into_response();
    };
    let Some(store) = hub.store() else {
        return (StatusCode::NOT_FOUND, "billing not configured").into_response();
    };
    // (1) Verify the Stripe-Signature HMAC over the raw body.
    let sig = headers.get("stripe-signature").and_then(|v| v.to_str().ok()).unwrap_or("");
    let now = now_secs();
    if !cc_screen_auth::stripe_signature_ok(sig, &body, &cfg.webhook_secret, now, SIG_TOLERANCE_SECS) {
        return (StatusCode::BAD_REQUEST, "bad signature").into_response();
    }
    let payload_hash = cc_screen_auth::sha256_hex_bytes(&body);
    // (2) Parse.
    let event: Event = match serde_json::from_slice(&body) {
        Ok(e) => e,
        Err(e) => {
            tracing::warn!("billing: webhook parse failed: {e}");
            return (StatusCode::BAD_REQUEST, "bad json").into_response();
        }
    };

    // Resolve (user_id, subscription) for the event, always re-fetching current
    // API truth so out-of-order delivery converges. `Ack` = ignore (200, no write).
    enum Resolved {
        Apply(String, Subscription),
        Ack,
        Upstream(anyhow::Error),
    }
    let resolved = match event.kind.as_str() {
        "checkout.session.completed" => {
            match serde_json::from_value::<CheckoutSessionObj>(event.data.object) {
                Ok(s) => match (s.client_reference_id, s.subscription) {
                    (Some(uid), Some(sub_id)) => match get_subscription(&cfg, &sub_id).await {
                        Ok(Some(sub)) => Resolved::Apply(uid, sub),
                        Ok(None) => Resolved::Ack,
                        Err(e) => Resolved::Upstream(e),
                    },
                    _ => Resolved::Ack,
                },
                Err(_) => Resolved::Ack,
            }
        }
        "customer.subscription.created"
        | "customer.subscription.updated"
        | "customer.subscription.deleted" => {
            match serde_json::from_value::<SubObj>(event.data.object) {
                Ok(o) => match get_subscription(&cfg, &o.id).await {
                    Ok(Some(sub)) => match store.user_by_billing_customer(&sub.customer).await {
                        Some(uid) => Resolved::Apply(uid, sub),
                        None => Resolved::Ack, // checkout event / reconcile heals
                    },
                    Ok(None) => Resolved::Ack,
                    Err(e) => Resolved::Upstream(e),
                },
                Err(_) => Resolved::Ack,
            }
        }
        "invoice.paid" | "invoice.payment_failed" => {
            match serde_json::from_value::<InvoiceObj>(event.data.object) {
                Ok(inv) => match inv.subscription_id() {
                    Some(sub_id) => match get_subscription(&cfg, &sub_id).await {
                        Ok(Some(sub)) => match store.user_by_billing_customer(&sub.customer).await {
                            Some(uid) => Resolved::Apply(uid, sub),
                            None => Resolved::Ack,
                        },
                        Ok(None) => Resolved::Ack,
                        Err(e) => Resolved::Upstream(e),
                    },
                    None => Resolved::Ack,
                },
                Err(_) => Resolved::Ack,
            }
        }
        // Unknown event types ack 200 (proposal 0058 B3).
        _ => Resolved::Ack,
    };

    let apply = match resolved {
        Resolved::Apply(uid, sub) => sub_to_apply(&uid, &sub, &cfg),
        Resolved::Ack => None,
        // Upstream Stripe failure → 502 (retry). 402 is reserved for plan limits.
        Resolved::Upstream(e) => {
            tracing::warn!("billing: webhook upstream fetch failed: {e}");
            return (StatusCode::BAD_GATEWAY, "payment provider error").into_response();
        }
    };
    // Nothing to persist (unknown type, unresolved user, or a skipped state): ack
    // WITHOUT recording the event id, so a redelivery once the state is resolvable
    // (e.g. subscription.created racing ahead of checkout) can still be processed.
    // Reconcile is the backstop.
    let Some(apply) = apply else {
        return (StatusCode::OK, "ok").into_response();
    };

    // (3) One transaction: idempotency insert + the state change. A duplicate
    // event id acks 200 with no change; a processing failure rolls the whole
    // transaction back (including the insert) so Stripe's retry reprocesses.
    match store.billing_process_event(&event.id, &payload_hash, now as i64, Some(apply)).await {
        Ok(_) => (StatusCode::OK, "ok").into_response(),
        Err(e) => {
            tracing::error!("billing: webhook apply failed for {}: {e}", event.id);
            (StatusCode::INTERNAL_SERVER_ERROR, "processing failed").into_response()
        }
    }
}

// ── nightly reconcile (proposal 0058 B5) ──────────────────────────────────────

/// Restore the webhook's at-least-once invariant: page every Stripe subscription
/// (`status=all`) and apply where local state differs, then downgrade any local
/// active/past_due row whose subscription is gone at Stripe. Logs divergence at
/// info; unresolvable paying customers loudly.
pub async fn reconcile(store: Arc<dyn Store>) {
    let Some(cfg) = BillingConfig::from_env() else { return };
    let mut fixed = 0usize;
    let rows = store.billing_rows_for_reconcile().await;

    // Stripe → local.
    let mut seen_subs: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut starting_after: Option<String> = None;
    loop {
        let page = match list_subscriptions(&cfg, starting_after.as_deref()).await {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!("reconcile: list subscriptions failed: {e}");
                break;
            }
        };
        for sub in &page.data {
            seen_subs.insert(sub.id.clone());
            let Some(uid) = store.user_by_billing_customer(&sub.customer).await else {
                tracing::warn!("reconcile: paying customer {} maps to no local user", sub.customer);
                continue;
            };
            if let Some(row) = rows.iter().find(|r| r.user_id == uid) {
                if let Some(apply) = reconcile_apply(row, Some(sub), &cfg) {
                    apply_reconcile(&store, &apply).await;
                    fixed += 1;
                    tracing::info!("reconcile: fixed user {uid} from Stripe sub {}", sub.id);
                }
            }
        }
        match page.data.last() {
            Some(last) if page.has_more => starting_after = Some(last.id.clone()),
            _ => break,
        }
    }

    // Local → Stripe: an active/past_due row whose subscription never showed up in
    // the status=all listing is gone at Stripe → downgrade.
    for row in &rows {
        let gone = match &row.subscription_id {
            Some(id) => !seen_subs.contains(id),
            None => true,
        };
        if gone {
            if let Some(apply) = reconcile_apply(row, None, &cfg) {
                apply_reconcile(&store, &apply).await;
                fixed += 1;
                tracing::info!("reconcile: downgraded user {} (subscription gone at Stripe)", row.user_id);
            }
        }
    }

    if fixed > 0 {
        tracing::info!("reconcile: applied {fixed} correction(s)");
    }
}

async fn apply_reconcile(store: &Arc<dyn Store>, a: &SubApply) {
    if let Err(e) = store
        .apply_subscription_state(
            &a.user_id,
            a.plan.as_deref(),
            &a.status,
            a.subscription_id.as_deref(),
            a.customer_id.as_deref(),
            a.period_end,
        )
        .await
    {
        tracing::warn!("reconcile: apply for {} failed: {e}", a.user_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{BillingRow, EventOutcome, SqliteStore, Store};

    fn cfg() -> BillingConfig {
        BillingConfig {
            secret_key: "sk_test".into(),
            webhook_secret: "whsec_test".into(),
            price_pro_monthly: "price_monthly".into(),
            price_pro_annual: "price_annual".into(),
            price_pro_founder: Some("price_founder".into()),
            founder_deadline: 2_000_000_000,
            public_url: "https://app.example.com".into(),
            managed_payments: false,
        }
    }

    fn sub(id: &str, status: &str, price: &str, customer: &str) -> Subscription {
        Subscription {
            id: id.into(),
            status: status.into(),
            current_period_end: Some(1_900_000_000),
            customer: customer.into(),
            items: SubItems {
                data: vec![SubItem { price: SubPrice { id: price.into() }, current_period_end: None }],
            },
        }
    }

    #[test]
    fn period_end_follows_basil_field_move() {
        // Pre-basil: top-level current_period_end.
        let mut s = sub("sub_1", "active", "price_monthly", "cus_1");
        assert_eq!(s.period_end(), Some(1_900_000_000));
        // basil: the field lives on the subscription item instead.
        s.current_period_end = None;
        s.items.data[0].current_period_end = Some(1_950_000_000);
        assert_eq!(s.period_end(), Some(1_950_000_000));
        let a = sub_to_apply("u", &s, &cfg()).unwrap();
        assert_eq!(a.period_end, Some(1_950_000_000));
    }

    #[test]
    fn invoice_subscription_id_accepts_both_shapes() {
        // Pre-basil shape.
        let old: InvoiceObj = serde_json::from_value(json!({ "subscription": "sub_old" })).unwrap();
        assert_eq!(old.subscription_id().as_deref(), Some("sub_old"));
        // basil shape: parent.subscription_details.subscription.
        let new: InvoiceObj = serde_json::from_value(json!({
            "parent": { "subscription_details": { "subscription": "sub_new" } }
        }))
        .unwrap();
        assert_eq!(new.subscription_id().as_deref(), Some("sub_new"));
        // Neither (a one-off invoice) → None → the webhook acks without a write.
        let none: InvoiceObj = serde_json::from_value(json!({ "parent": null })).unwrap();
        assert_eq!(none.subscription_id(), None);
    }

    #[test]
    fn founder_price_gate_is_server_side() {
        let c = cfg();
        let before = c.founder_deadline - 1;
        let after = c.founder_deadline + 1;
        // Beta user before the deadline, monthly → founder price.
        assert_eq!(resolve_checkout_price("beta", "pro-monthly", before, &c).as_deref(), Some("price_founder"));
        // Beta user after the deadline → standard monthly.
        assert_eq!(resolve_checkout_price("beta", "pro-monthly", after, &c).as_deref(), Some("price_monthly"));
        // Free user (any time) → standard monthly, never founder.
        assert_eq!(resolve_checkout_price("free", "pro-monthly", before, &c).as_deref(), Some("price_monthly"));
        // Annual is always the annual price (founder is a monthly lock).
        assert_eq!(resolve_checkout_price("beta", "pro-annual", before, &c).as_deref(), Some("price_annual"));
        // Unknown choice → None (400).
        assert_eq!(resolve_checkout_price("beta", "garbage", before, &c), None);
        // No founder price configured → beta falls back to standard monthly.
        let mut c2 = cfg();
        c2.price_pro_founder = None;
        assert_eq!(resolve_checkout_price("beta", "pro-monthly", before, &c2).as_deref(), Some("price_monthly"));
    }

    #[test]
    fn plan_intent_maps_statuses() {
        assert_eq!(plan_intent("active"), Some((Some("pro"), "active")));
        assert_eq!(plan_intent("trialing"), Some((Some("pro"), "active")));
        assert_eq!(plan_intent("past_due"), Some((Some("pro"), "past_due")));
        assert_eq!(plan_intent("unpaid"), Some((Some("pro"), "past_due")));
        assert_eq!(plan_intent("canceled"), Some((None, "canceled")));
        assert_eq!(plan_intent("incomplete_expired"), Some((None, "canceled")));
        assert_eq!(plan_intent("incomplete"), None);
    }

    #[test]
    fn sub_to_apply_requires_known_price_for_paid() {
        let c = cfg();
        // Recognized price → pro apply with the sub id set.
        let a = sub_to_apply("u", &sub("sub_1", "active", "price_monthly", "cus_1"), &c).unwrap();
        assert_eq!(a.plan.as_deref(), Some("pro"));
        assert_eq!(a.status, "active");
        assert_eq!(a.subscription_id.as_deref(), Some("sub_1"));
        assert_eq!(a.customer_id.as_deref(), Some("cus_1"));
        // Unrecognized price on an active sub → skip (None).
        assert!(sub_to_apply("u", &sub("sub_1", "active", "price_bogus", "cus_1"), &c).is_none());
        // Canceled → terminal: plan None, sub id cleared, customer kept.
        let d = sub_to_apply("u", &sub("sub_1", "canceled", "price_bogus", "cus_1"), &c).unwrap();
        assert_eq!(d.plan, None);
        assert_eq!(d.status, "canceled");
        assert_eq!(d.subscription_id, None);
        assert_eq!(d.customer_id.as_deref(), Some("cus_1"));
    }

    #[test]
    fn reconcile_apply_heals_and_noops() {
        let c = cfg();
        // Local pro/active, Stripe canceled → heal (terminal apply).
        let row = BillingRow {
            user_id: "u".into(),
            customer_id: Some("cus_1".into()),
            subscription_id: Some("sub_1".into()),
            plan: "pro".into(),
            plan_status: Some("active".into()),
        };
        let canceled = sub("sub_1", "canceled", "price_monthly", "cus_1");
        assert!(reconcile_apply(&row, Some(&canceled), &c).is_some(), "diverged → write");
        // Local already canceled + Stripe canceled → no write.
        let row2 = BillingRow { plan: "beta".into(), plan_status: Some("canceled".into()), ..row.clone() };
        assert!(reconcile_apply(&row2, Some(&canceled), &c).is_none(), "in sync → zero writes");
        // Local pro/active + Stripe active(pro) → no write.
        let active = sub("sub_1", "active", "price_monthly", "cus_1");
        assert!(reconcile_apply(&row, Some(&active), &c).is_none());
        // Subscription gone at Stripe while local still active → downgrade.
        assert!(reconcile_apply(&row, None, &c).is_some());
        // Gone + already canceled locally → no write.
        assert!(reconcile_apply(&row2, None, &c).is_none());
    }

    // Event dispatch against a real store: happy path + idempotency + out-of-order
    // convergence (acceptance 7, 8), exercising sub_to_apply → billing_process_event
    // the same way the webhook does (minus the network fetch it mocks here).
    #[tokio::test]
    async fn dispatch_happy_idempotent_and_out_of_order() {
        let c = cfg();
        let s = SqliteStore::in_memory().await;
        let uid = s.create_user("u@x.com", "password12345").await.unwrap();
        s.set_plan("u@x.com", "beta").await.unwrap();

        // checkout.session.completed → fetched active pro subscription.
        let sub_active = sub("sub_1", "active", "price_monthly", "cus_1");
        let a = sub_to_apply(&uid, &sub_active, &c).unwrap();
        assert_eq!(s.billing_process_event("evt_1", "h", 1, Some(a.clone())).await.unwrap(), EventOutcome::Applied);
        assert_eq!(s.limits_for(&uid).await.plan, "pro");
        assert_eq!(s.limits_for(&uid).await.max_agents, 100, "Pro caps, no Stripe call on the read path");

        // Replay → duplicate, no double-apply.
        assert_eq!(s.billing_process_event("evt_1", "h", 2, Some(a)).await.unwrap(), EventOutcome::Duplicate);

        // deleted → terminal restore to beta.
        let sub_gone = sub("sub_1", "canceled", "price_monthly", "cus_1");
        let del = sub_to_apply(&uid, &sub_gone, &c).unwrap();
        s.billing_process_event("evt_2", "h", 3, Some(del)).await.unwrap();
        assert_eq!(s.limits_for(&uid).await.plan, "beta");

        // Out-of-order: a late 'updated' whose re-fetch STILL says canceled → stays
        // beta (no resurrection of pro), because we apply current API truth.
        let late = sub_to_apply(&uid, &sub("sub_1", "canceled", "price_monthly", "cus_1"), &c).unwrap();
        s.billing_process_event("evt_3", "h", 4, Some(late)).await.unwrap();
        assert_eq!(s.limits_for(&uid).await.plan, "beta");
    }

    // NB: the webhook's signature verification (acceptance 6's signature half) is
    // covered by `stripe_signature_ok`'s vectors in the cc-screen-auth crate — the
    // hub deliberately doesn't pull in hmac/sha2 to recompute one here.
}

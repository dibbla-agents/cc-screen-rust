//! Read-only link grants over the real router (proposal 0083 Part C).
//!
//! A scriptable fake agent dials the real multi-tenant hub; the owner mints a
//! link with their login cookie; an **anonymous** client — no cookie, no
//! bearer, nothing — reads the file through `/api/link/:token/content`. What
//! this file is really for is the refusals: that revoked, expired, unknown and
//! malformed are one indistinguishable answer, that nothing anywhere takes this
//! token for a write, and that the grant is invisible to every other surface.
#![cfg(all(feature = "test-support", feature = "multi-tenant"))]

use std::time::Duration;

use cc_screen_hub::db::Store as _;
use cc_screen_hub::test_support::{sess, spawn_scriptable_agent, start_hub_multi, MultiHub};

const FILE: &str = "/home/owner/projects/personal-planning/tasks.md";

/// Owner + an online agent labelled `studio`, with a login cookie.
async fn setup() -> (MultiHub, String, String) {
    let hub = start_hub_multi().await;
    let (uid, cookie) = hub.user_with_cookie("owner@x.com").await;
    let (token, _agent_id) = hub.store.upsert_agent(&uid, "studio").await.unwrap();
    spawn_scriptable_agent(&hub.addr, "studio", Some(&token), vec![sess("claude-a")]).await;
    // Give the uplink a moment to register before the first resolve.
    for _ in 0..100 {
        if !hub.store.user_email(&uid).await.is_none() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    await_agent(&hub, &cookie).await;
    (hub, uid, cookie)
}

/// Block until the agent shows up in the owner's machine list.
async fn await_agent(hub: &MultiHub, cookie: &str) {
    let client = reqwest::Client::new();
    for _ in 0..200 {
        let r = client
            .get(format!("http://{}/api/machines", hub.addr))
            .header("cookie", cookie)
            .send()
            .await
            .unwrap();
        let v: serde_json::Value = r.json().await.unwrap();
        if v.as_array().map(|a| !a.is_empty()).unwrap_or(false) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("agent never registered");
}

/// Mint a link for `path`; returns `(status, body)`.
async fn mint(hub: &MultiHub, cookie: &str, path: &str) -> (u16, serde_json::Value) {
    let r = reqwest::Client::new()
        .post(format!("http://{}/api/shares", hub.addr))
        .header("cookie", cookie)
        .json(&serde_json::json!({ "kind": "link", "machine": "studio", "path": path }))
        .send()
        .await
        .unwrap();
    let status = r.status().as_u16();
    let body = r.json::<serde_json::Value>().await.unwrap_or(serde_json::Value::Null);
    (status, body)
}

/// The token out of a mint response's `invite_url` (`…/s/<token>`).
fn token_of(body: &serde_json::Value) -> String {
    body["invite_url"].as_str().unwrap().rsplit('/').next().unwrap().to_string()
}

/// A full anonymous response: status + the headers we contract on + body.
async fn anon(hub: &MultiHub, path: &str) -> (u16, Vec<(String, String)>, String) {
    let r = reqwest::Client::new().get(format!("http://{}{path}", hub.addr)).send().await.unwrap();
    let status = r.status().as_u16();
    let headers = r
        .headers()
        .iter()
        .map(|(k, v)| (k.as_str().to_string(), v.to_str().unwrap_or_default().to_string()))
        .filter(|(k, _)| k != "content-length" && k != "date")
        .collect::<Vec<_>>();
    (status, headers, r.text().await.unwrap_or_default())
}

#[tokio::test]
async fn mint_read_anonymously_then_revoke() {
    let (hub, _uid, cookie) = setup().await;

    let (status, body) = mint(&hub, &cookie, FILE).await;
    assert_eq!(status, 200, "mint: {body}");
    assert_eq!(body["status"], "active");
    let token = token_of(&body);
    let id = body["id"].as_str().unwrap().to_string();

    // The token is hashed at rest, so it appears NOWHERE in the outbox.
    let outbox = reqwest::Client::new()
        .get(format!("http://{}/api/shares/outbox", hub.addr))
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(!outbox.contains(&token), "the outbox must never carry the token");
    assert!(outbox.contains("tasks.md"), "but it does list the file: {outbox}");

    // Anonymous read — no cookie, no bearer — returns the bytes under the
    // full response contract.
    let (status, headers, text) = anon(&hub, &format!("/api/link/{token}/content")).await;
    assert_eq!(status, 200);
    assert_eq!(text, "fake-agent file body");
    let h = |name: &str| headers.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone());
    assert_eq!(h("content-type").as_deref(), Some("text/plain; charset=utf-8"));
    assert_eq!(h("x-content-type-options").as_deref(), Some("nosniff"));
    assert_eq!(h("cache-control").as_deref(), Some("no-store"));
    assert_eq!(h("referrer-policy").as_deref(), Some("no-referrer"));
    assert_eq!(h("x-robots-tag").as_deref(), Some("noindex, nofollow"));
    assert!(h("etag").is_none(), "no validators: a 304 would outlive a revoke");
    assert!(h("last-modified").is_none());

    // The metadata endpoint is answered by the hub alone.
    let (status, _h, text) = anon(&hub, &format!("/api/link/{token}")).await;
    assert_eq!(status, 200);
    let meta: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(meta["name"], "tasks.md");
    assert_eq!(meta["machine"], "studio");
    assert_eq!(meta["mimeClass"], "markdown");

    // Revoke through the ordinary share endpoint, then it is gone.
    let r = reqwest::Client::new()
        .post(format!("http://{}/api/shares/{id}/revoke", hub.addr))
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 200);
    let (status, _, _) = anon(&hub, &format!("/api/link/{token}/content")).await;
    assert_eq!(status, 404, "revoked ⇒ gone");
}

#[tokio::test]
async fn every_refusal_is_byte_identical() {
    let (hub, _uid, cookie) = setup().await;
    let (_s, body) = mint(&hub, &cookie, FILE).await;
    let token = token_of(&body);
    let id = body["id"].as_str().unwrap().to_string();

    // An expired grant (written straight to the store — there is no HTTP way to
    // travel in time) and a revoked one and an unknown one and a malformed one.
    let (_s, expired_body) = mint(&hub, &cookie, "/home/owner/other.md").await;
    let expired_token = token_of(&expired_body);
    sqlx::query("UPDATE link_shares SET expires_at = 1 WHERE id = ?1")
        .bind(expired_body["id"].as_str().unwrap())
        .execute(hub.store.pool())
        .await
        .unwrap();

    reqwest::Client::new()
        .post(format!("http://{}/api/shares/{id}/revoke", hub.addr))
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap();

    let unknown = "A".repeat(43);
    let malformed = "nope";
    let mut seen = Vec::new();
    for t in [token.as_str(), expired_token.as_str(), unknown.as_str(), malformed] {
        seen.push(anon(&hub, &format!("/api/link/{t}/content")).await);
        seen.push(anon(&hub, &format!("/api/link/{t}")).await);
    }
    let first = seen[0].clone();
    for (i, got) in seen.iter().enumerate() {
        assert_eq!(got.0, 404, "refusal #{i} status");
        assert_eq!(got.1, first.1, "refusal #{i} headers differ — that is an oracle");
        assert_eq!(got.2, first.2, "refusal #{i} body differs — that is an oracle");
    }
}

#[tokio::test]
async fn the_token_is_not_a_credential_anywhere_else() {
    let (hub, _uid, cookie) = setup().await;
    let (_s, body) = mint(&hub, &cookie, FILE).await;
    let token = token_of(&body);
    let client = reqwest::Client::new();

    // Every shape someone might hope works: the token as a bearer, as a cookie,
    // as a query parameter — against the write surface, the listing surface and
    // the session surface. All refused; none of them 200.
    let write = client
        .post(format!("http://{}/api/file/write?machine=studio", hub.addr))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "path": FILE, "content": "pwned", "baseMtime": 0 }))
        .send()
        .await
        .unwrap();
    assert_eq!(write.status().as_u16(), 401, "no write with a link token");

    for (path, header_token) in [
        ("/api/files?machine=studio", true),
        ("/api/sessions", true),
        (&format!("/api/file/read?machine=studio&path={FILE}") as &str, true),
        (&format!("/api/file/read?machine=studio&token={token}&path={FILE}") as &str, false),
    ] {
        let mut req = client.get(format!("http://{}{path}", hub.addr));
        if header_token {
            req = req.bearer_auth(&token);
        }
        let status = req.send().await.unwrap().status().as_u16();
        assert_eq!(status, 401, "{path} must not accept a link token");
    }

    // And there is no `/api/link/:token/…` beyond the two reads.
    let post = client
        .post(format!("http://{}/api/link/{token}/content", hub.addr))
        .body("x")
        .send()
        .await
        .unwrap();
    assert!(post.status().as_u16() >= 400, "content is GET-only");
}

#[tokio::test]
async fn minting_is_owner_only_and_needs_a_live_machine() {
    let (hub, _uid, _cookie) = setup().await;
    let (_other, other_cookie) = hub.user_with_cookie("stranger@x.com").await;

    // A stranger cannot publish a file off someone else's machine, even naming
    // it exactly — the resolve runs under their OWN scope.
    let (status, _b) = mint(&hub, &other_cookie, FILE).await;
    assert_eq!(status, 404, "not your machine");

    // Anonymous mint is refused by the auth gate before the handler.
    let status = reqwest::Client::new()
        .post(format!("http://{}/api/shares", hub.addr))
        .json(&serde_json::json!({ "kind": "link", "machine": "studio", "path": FILE }))
        .send()
        .await
        .unwrap()
        .status()
        .as_u16();
    assert_eq!(status, 401);
}

#[tokio::test]
async fn regenerate_kills_the_old_url_and_keeps_the_grant() {
    let (hub, _uid, cookie) = setup().await;
    let (_s, body) = mint(&hub, &cookie, FILE).await;
    let old = token_of(&body);
    let id = body["id"].as_str().unwrap().to_string();

    let r = reqwest::Client::new()
        .post(format!("http://{}/api/shares/{id}/regenerate", hub.addr))
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 200);
    let fresh: serde_json::Value = r.json().await.unwrap();
    let new = token_of(&fresh);
    assert_ne!(old, new);
    assert_eq!(fresh["id"], id, "same grant");

    assert_eq!(anon(&hub, &format!("/api/link/{old}/content")).await.0, 404, "old URL dies");
    assert_eq!(anon(&hub, &format!("/api/link/{new}/content")).await.0, 200, "new URL serves");

    // A stranger cannot rotate someone else's link.
    let (_o, other_cookie) = hub.user_with_cookie("stranger2@x.com").await;
    let status = reqwest::Client::new()
        .post(format!("http://{}/api/shares/{id}/regenerate", hub.addr))
        .header("cookie", &other_cookie)
        .send()
        .await
        .unwrap()
        .status()
        .as_u16();
    assert_eq!(status, 404);
}

#[tokio::test]
async fn a_link_grant_shows_up_in_no_other_list() {
    let (hub, _uid, cookie) = setup().await;
    mint(&hub, &cookie, FILE).await;
    let (_stranger, other_cookie) = hub.user_with_cookie("nosy@x.com").await;
    let client = reqwest::Client::new();

    for who in [&cookie, &other_cookie] {
        for path in ["/api/shares/inbox", "/api/shares/received"] {
            let text = client
                .get(format!("http://{}{path}", hub.addr))
                .header("cookie", who)
                .send()
                .await
                .unwrap()
                .text()
                .await
                .unwrap();
            assert_eq!(text.trim(), "[]", "{path} must not see a link grant: {text}");
        }
    }
    // And it grants the stranger no machine.
    let machines = client
        .get(format!("http://{}/api/machines", hub.addr))
        .header("cookie", &other_cookie)
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert_eq!(machines.trim(), "[]");
}

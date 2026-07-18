//! HTTP-layer integration tests for the public quota fetchers.
//!
//! Every request goes to a local `httpmock` server whose URL is injected via the
//! endpoint parameters added for testability, so no real provider API is ever
//! reached. Private fetchers / orchestration (Claude, Cursor, Copilot, the
//! 401 → refresh → retry loop, GitHub releases) are covered by inline unit tests
//! in their own source files, which can see crate-private items.

use httpmock::prelude::*;
use serde_json::json;
use vct_test_support::fixture_str;
use vibe_coding_tracker::quota::http::build_client;
use vibe_coding_tracker::quota::wham::{
    ResetCreditsResult, WhamResult, call_reset_credit_details, call_wham,
    call_wham_with_reset_credits, refresh_codex,
};

#[test]
fn call_wham_maps_200_response() {
    let server = MockServer::start();
    let endpoint = server.mock(|when, then| {
        when.method(GET).path("/wham");
        then.status(200)
            .header("content-type", "application/json")
            .body(fixture_str("quota/wham_usage_response.json"));
    });
    let client = build_client().unwrap();

    let result = call_wham(
        &client,
        "tok",
        Some("acct"),
        1_000_000,
        &server.url("/wham"),
    );
    endpoint.assert();

    match result {
        WhamResult::Ok(snap) => {
            assert_eq!(snap.plan_type.as_deref(), Some("plus"));
            assert_eq!(snap.primary.as_ref().unwrap().used_percent, 27.0);
            assert_eq!(snap.secondary.as_ref().unwrap().used_percent, 4.0);
        }
        _ => panic!("expected WhamResult::Ok"),
    }
}

#[test]
fn call_wham_401_is_unauthorized() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(GET).path("/wham");
        then.status(401);
    });
    let client = build_client().unwrap();

    let result = call_wham(&client, "tok", None, 0, &server.url("/wham"));
    assert!(matches!(result, WhamResult::Unauthorized));
}

#[test]
fn call_wham_500_is_transient() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(GET).path("/wham");
        then.status(500);
    });
    let client = build_client().unwrap();

    let result = call_wham(&client, "tok", None, 0, &server.url("/wham"));
    assert!(matches!(result, WhamResult::Transient));
}

#[test]
fn call_wham_with_reset_credits_maps_details() {
    let server = MockServer::start();
    let usage = server.mock(|when, then| {
        when.method(GET).path("/wham");
        then.status(200)
            .header("content-type", "application/json")
            .body(fixture_str("quota/wham_usage_response.json"));
    });
    let details = server.mock(|when, then| {
        when.method(GET)
            .path("/reset-credits")
            .header("authorization", "Bearer tok")
            .header("chatgpt-account-id", "acct")
            .header("originator", "codex_cli_rs");
        then.status(200)
            .header("content-type", "application/json")
            .body(fixture_str(
                "quota/wham_rate_limit_reset_credits_response.json",
            ));
    });
    let client = build_client().unwrap();

    let result = call_wham_with_reset_credits(
        &client,
        "tok",
        Some("acct"),
        1_000_000,
        &server.url("/wham"),
        &server.url("/reset-credits"),
    );

    usage.assert();
    details.assert();
    match result {
        WhamResult::Ok(snap) => {
            assert_eq!(snap.reset_credits_available, Some(5));
            let expirations = snap.reset_credit_expirations.unwrap();
            assert_eq!(expirations.len(), 3);
            assert!(expirations[0].unwrap() < expirations[1].unwrap());
            assert_eq!(expirations[2], None);
        }
        _ => panic!("expected WhamResult::Ok"),
    }
}

#[test]
fn reset_credit_details_failure_preserves_usage_summary() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(GET).path("/wham");
        then.status(200)
            .body(fixture_str("quota/wham_usage_response.json"));
    });
    server.mock(|when, then| {
        when.method(GET).path("/reset-credits");
        then.status(500).body("boom");
    });
    let client = build_client().unwrap();

    let result = call_wham_with_reset_credits(
        &client,
        "tok",
        Some("acct"),
        1_000_000,
        &server.url("/wham"),
        &server.url("/reset-credits"),
    );

    match result {
        WhamResult::Ok(snap) => {
            assert_eq!(snap.reset_credits_available, Some(2));
            assert!(snap.reset_credit_expirations.is_none());
        }
        _ => panic!("details failure must not fail the usage snapshot"),
    }
}

#[test]
fn reset_credit_details_401_is_unauthorized() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(GET).path("/reset-credits");
        then.status(401);
    });
    let client = build_client().unwrap();

    let result =
        call_reset_credit_details(&client, "tok", Some("acct"), &server.url("/reset-credits"));

    assert!(matches!(result, ResetCreditsResult::Unauthorized));
}

#[test]
fn refresh_codex_rotates_and_writes_back_token() {
    let server = MockServer::start();
    let endpoint = server.mock(|when, then| {
        when.method(POST).path("/token");
        then.status(200).json_body(json!({
            "access_token": "new-access-token",
            "refresh_token": "new-refresh-token"
        }));
    });
    let dir = tempfile::tempdir().unwrap();
    let auth = dir.path().join("auth.json");
    std::fs::write(
        &auth,
        json!({ "tokens": { "refresh_token": "old-refresh-token" } }).to_string(),
    )
    .unwrap();

    let client = build_client().unwrap();
    let access =
        refresh_codex(&client, &auth, &server.url("/token")).expect("refresh should succeed");

    endpoint.assert();
    assert_eq!(access, "new-access-token");

    // The rotated tokens must be persisted back into auth.json.
    let written: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&auth).unwrap()).unwrap();
    assert_eq!(written["tokens"]["access_token"], "new-access-token");
    assert_eq!(written["tokens"]["refresh_token"], "new-refresh-token");
}

#[test]
fn refresh_codex_errors_on_non_success_status() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(POST).path("/token");
        then.status(400)
            .json_body(json!({ "error": "invalid_grant" }));
    });
    let dir = tempfile::tempdir().unwrap();
    let auth = dir.path().join("auth.json");
    std::fs::write(
        &auth,
        json!({ "tokens": { "refresh_token": "stale" } }).to_string(),
    )
    .unwrap();

    let client = build_client().unwrap();
    assert!(refresh_codex(&client, &auth, &server.url("/token")).is_err());
}

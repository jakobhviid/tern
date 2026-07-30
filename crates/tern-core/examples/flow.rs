//! Runnable end-to-end demonstration of the orchestration engine against a mock UCS server + the in-memory
//! stub backend — so the whole flow can be *seen* on any platform (macOS/CI) with no display, D-Bus, or real
//! UniFi account. Run with:
//!
//! ```sh
//! cargo run -p tern-core --example flow
//! ```
//!
//! Uses `wiremock` (a dev-dependency), so it never ships in a production binary.

use std::sync::Arc;

use tern_core::backend::StubBackend;
use tern_core::config::Config;
use tern_core::engine::Engine;
use tern_core::error::Error;
use tern_core::ucs::{Endpoints, UcsClient};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let server = start_mock_ucs().await;

    // Build the engine: real UCS client (pointed at the mock) + one stub backend serving all four seams.
    let ucs = UcsClient::new(Endpoints { sso: "https://sso.ui.com".into(), api_gw: server.uri() });
    let stub = Arc::new(StubBackend::new());
    let mut config = Config::default();
    config.set_auto_mount("d1", true); // the user ticked "Design" for auto-mount; left "Archive" off

    let mut engine = Engine::new(ucs, stub.clone(), stub.clone(), stub.clone(), stub.clone(), stub, config);

    println!("== tern flow demo (mock UCS + stub backend) ==\n");
    print_snapshot("start", &engine).await;

    println!("\n→ sign in (browser SSO already returned a bearer token)");
    engine.sign_in("demo-jwt".into()).await.expect("sign in");
    print_snapshot("signed in", &engine).await;

    println!("\n→ turn on Access for console 'c1' (provision vpn/session → bring up tunnel → auto-mount)");
    engine.connect("c1").await.expect("connect");
    print_snapshot("access on", &engine).await;

    println!("\n→ turn off Access");
    engine.disconnect().await.expect("disconnect");
    print_snapshot("access off", &engine).await;

    // Show how a couple of failures render for a non-technical user (docs/05).
    println!("\n== error rendering (what the user actually sees) ==");
    for e in [
        Error::SessionExpired,
        Error::VpnUnreachable,
        Error::DriveUnreachable,
        Error::AccountRestricted("your account has been locked".into()),
        Error::RelayOnly,
    ] {
        let uf = e.user_facing();
        let detail = uf.detail.map(|d| format!("  (support detail: {d})")).unwrap_or_default();
        println!("  • {}  [{}]{}", uf.title, uf.action.label(), detail);
    }
}

async fn print_snapshot(label: &str, engine: &Engine) {
    let s = engine.snapshot().await;
    println!("[{label}] {}", s.summary_line());
    for d in &s.drives {
        println!("    - {:<10} {}", d.drive.name, d.state.label());
    }
}

async fn start_mock_ucs() -> MockServer {
    let s = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/proxy/users/public/api/v2/identity/info"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "email": "jah@example.com", "displayName": "Jakob", "organization": "Acme"
        })))
        .mount(&s)
        .await;
    Mock::given(method("GET"))
        .and(path("/user-token/hosts/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            {"consoleStandardId": "c1", "hostName": "Home"}
        ])))
        .mount(&s)
        .await;
    Mock::given(method("POST"))
        .and(path("/proxy/users/public/api/v2/identity/public_key"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&s)
        .await;
    Mock::given(method("POST"))
        .and(path("/proxy/ucs/public/user/api/v1/vpn/session"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "sessionId": "sess-1",
            "wgConfig": {
                "serverPublicKey": "srv", "endpoint": "203.0.113.7:51820",
                "allowedIps": ["10.0.0.0/8"], "persistentKeepalive": 25, "clientAddress": ["10.2.0.9/32"]
            }
        })))
        .mount(&s)
        .await;
    Mock::given(method("GET"))
        .and(path("/proxy/ucs/public/user/api/v1/drive/list"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            {"id": "d1", "name": "Design"},
            {"id": "d2", "name": "Archive"}
        ])))
        .mount(&s)
        .await;
    s
}

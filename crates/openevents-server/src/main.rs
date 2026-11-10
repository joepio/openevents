use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use openevents_core::kv::RedbStore;
use openevents_core::{Actor, Register};
use openevents_server::auth::{AuthConfig, Tickets};
use openevents_server::{router, AppState};
use tower_http::cors::CorsLayer;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let data_dir =
        PathBuf::from(std::env::var("OPENEVENTS_DATA").unwrap_or_else(|_| "data".into()));
    std::fs::create_dir_all(&data_dir).expect("create data dir");
    let store = RedbStore::open(&data_dir.join("openevents.redb")).expect("open store");
    let register = Arc::new(Register::open(Arc::new(store)).expect("open register"));

    // Profiles in OPENEVENTS_PROFILES (a directory of .json files) are installed at start.
    if let Ok(dir) = std::env::var("OPENEVENTS_PROFILES") {
        let system = Actor {
            id: "urn:openevents:system".into(),
            claims: serde_json::json!({}),
            admin: true,
        };
        for entry in std::fs::read_dir(&dir)
            .expect("read profiles dir")
            .flatten()
        {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let raw: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&path).expect("read profile"))
                    .expect("profile is JSON");
            let id = raw["id"].as_str().unwrap_or_default().to_string();
            match register.put_profile(&system, &id, raw) {
                Ok(_) => tracing::info!("profile {id} installed from {}", path.display()),
                Err(e) => panic!("profile {} is invalid: {e:?}", path.display()),
            }
        }
    }

    let auth = AuthConfig::from_env();
    if let Some(oidc) = &auth.oidc {
        match oidc.refresh().await {
            Ok(n) => tracing::info!("OIDC: {n} signing keys loaded"),
            // Not fatal: the IdP may come up later; keys are fetched on the first token.
            Err(e) => tracing::warn!("OIDC: could not load keys yet: {e}"),
        }
    }
    if auth.jwt_secret.is_none() && !auth.dev_auth && auth.oidc.is_none() {
        tracing::warn!("no OIDC, no OPENEVENTS_JWT_SECRET and no OPENEVENTS_DEV_AUTH: every request will be unauthorized");
    }
    let state = AppState {
        register,
        auth,
        tickets: Arc::new(Tickets::default()),
        notificaties: openevents_server::notificaties::NotificatiesConfig::from_env().map(Arc::new),
    };
    let app = router(state).layer(CorsLayer::permissive());

    let addr: SocketAddr = std::env::var("OPENEVENTS_ADDR")
        .unwrap_or_else(|_| "0.0.0.0:8080".into())
        .parse()
        .expect("OPENEVENTS_ADDR");
    tracing::info!("OpenEvents listening on {addr}");
    let listener = tokio::net::TcpListener::bind(addr).await.expect("bind");
    axum::serve(listener, app).await.expect("serve");
}

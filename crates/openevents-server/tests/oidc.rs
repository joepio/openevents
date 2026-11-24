//! Tokens from an OpenID Connect provider: a supplier with OAuth2 client credentials.
//! A small fake IdP serves the discovery document and the JWKS.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::{routing::get, Json, Router};
use http_body_util::BodyExt;
use jsonwebtoken::{encode, EncodingKey, Header};
use openevents_core::kv::MemoryStore;
use openevents_core::Register;
use openevents_server::auth::{AuthConfig, Oidc, Tickets};
use openevents_server::{router, AppState};
use serde_json::{json, Value};
use tower::ServiceExt;

const KEY: &[u8] = include_bytes!("keys/test-rsa.pem");
const JWKS: &str = include_str!("keys/test-jwks.json");

async fn fake_idp() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let issuer = base.clone();
    let app = Router::new()
        .route(
            "/.well-known/openid-configuration",
            get(move || async move {
                Json(json!({ "issuer": issuer, "jwks_uri": format!("{issuer}/jwks") }))
            }),
        )
        .route(
            "/jwks",
            get(|| async { Json(serde_json::from_str::<Value>(JWKS).unwrap()) }),
        );
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    base
}

fn app(issuer: &str) -> Router {
    let register = Arc::new(Register::open(Arc::new(MemoryStore::new())).unwrap());
    let auth = AuthConfig {
        admins: vec!["urn:admin".into()],
        oidc: Some(Arc::new(Oidc::new(
            Some(issuer.to_string()),
            None,
            Some("openevents".into()),
        ))),
        actor_claim: "azp".into(),
        dev_auth: true, // only to install the profile as admin
        ..Default::default()
    };
    router(AppState {
        register,
        auth,
        tickets: Arc::new(Tickets::default()),
        schemas: Arc::new(openevents_server::schemas::Schemas::new(Some(
            concat!(env!("CARGO_MANIFEST_DIR"), "/../../examples/schemas").into(),
        ))),
        notificaties: vec![],
    })
}

fn token(issuer: &str, kid: &str, aud: &str, exp_offset: i64) -> String {
    let mut h = Header::new(jsonwebtoken::Algorithm::RS256);
    h.kid = Some(kid.into());
    let now = chrono::Utc::now().timestamp();
    let claims = json!({
        "iss": issuer, "aud": aud, "sub": "6f1c0d2e-service-account", "azp": "signalen",
        "rol": "systeem", "iat": now, "exp": now + exp_offset
    });
    encode(&h, &claims, &EncodingKey::from_rsa_pem(KEY).unwrap()).unwrap()
}

async fn post(app: &Router, token: &str, path: &str, body: Value) -> (StatusCode, Value) {
    let req = Request::builder()
        .method(if path.starts_with("/profiles") {
            "PUT"
        } else {
            "POST"
        })
        .uri(path)
        .header("authorization", format!("Bearer {token}"))
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

fn melding() -> Value {
    json!({
        "specversion": "1.0", "id": "m1", "source": "https://signalen.demodam.nl",
        "type": "nl.vng.meldingen.melding.gedaan", "subject": "https://zaken.demodam.nl/meldingen/MOR-1",
        "data": { "resourceData": {
            "zaaknummer": "MOR-1", "categorie": "Afval", "omschrijving": "Volle container", "team": "afval",
            "locatie": { "type": "Point", "coordinates": [5.12, 52.09] },
            "melder": { "partij": "https://id.example.nl/priya", "rol": "inwoner" }
        } }
    })
}

#[tokio::test]
async fn client_credentials_token_from_idp() {
    let issuer = fake_idp().await;
    let app = app(&issuer);
    let profile: Value =
        serde_json::from_str(include_str!("../../../examples/profiles/meldingen.json")).unwrap();
    let (s, _) = post(&app, "dev:urn:admin", "/profiles/nl.vng.meldingen", profile).await;
    assert_eq!(s, StatusCode::OK);

    // Keys are fetched lazily on the first token (discovery, then JWKS).
    let good = token(&issuer, "test-1", "openevents", 300);
    let (s, b) = post(&app, &good, "/events", melding()).await;
    assert_eq!(s, StatusCode::CREATED, "{b}");

    let mut again = melding();
    again["id"] = json!("m2");
    for (bad, why) in [
        (
            token(&issuer, "test-1", "iets-anders", 300),
            "wrong audience",
        ),
        (
            token("https://andere-idp.example", "test-1", "openevents", 300),
            "wrong issuer",
        ),
        (token(&issuer, "test-1", "openevents", -600), "expired"),
        (token(&issuer, "onbekend", "openevents", 300), "unknown key"),
    ] {
        let (s, _) = post(&app, &bad, "/events", again.clone()).await;
        assert_eq!(s, StatusCode::UNAUTHORIZED, "{why}");
    }
}

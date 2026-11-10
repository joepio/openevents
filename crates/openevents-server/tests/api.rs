use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use openevents_core::kv::{KvStore, MemoryStore, RedbStore};
use openevents_core::Register;
use openevents_server::auth::{AuthConfig, Tickets};
use openevents_server::{router, AppState};
use serde_json::{json, Value};
use tower::ServiceExt;

const ADMIN: &str = "dev:urn:admin";
const SUZANNE: &str = "dev:https://id.example.nl/suzanne";
const KAREL: &str = "dev:https://id.example.nl/karel";
const MERVE: &str = "dev:https://id.example.nl/merve|team=wmo";
const SUZANNE_ID: &str = "https://id.example.nl/suzanne";

fn app_with(kv: Arc<dyn KvStore>) -> Router {
    let register = Arc::new(Register::open(kv).unwrap());
    let auth = AuthConfig {
        jwt_secret: Some("test-secret".into()),
        dev_auth: true,
        admins: vec!["urn:admin".into()],
    };
    router(AppState {
        register,
        auth,
        tickets: Arc::new(Tickets::default()),
    })
}

fn app() -> Router {
    app_with(Arc::new(MemoryStore::new()))
}

async fn call(
    app: &Router,
    method: &str,
    uri: &str,
    token: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, Value, axum::http::HeaderMap) {
    let mut req = Request::builder().method(method).uri(uri);
    if let Some(t) = token {
        req = req.header("authorization", format!("Bearer {t}"));
    }
    let req = match body {
        Some(b) => req
            .header("content-type", "application/json")
            .body(Body::from(b.to_string()))
            .unwrap(),
        None => req.body(Body::empty()).unwrap(),
    };
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let headers = res.headers().clone();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let v = serde_json::from_slice(&bytes)
        .unwrap_or(Value::String(String::from_utf8_lossy(&bytes).into()));
    (status, v, headers)
}

fn profile(name: &str) -> Value {
    let path = format!(
        "{}/../../examples/profiles/{name}.json",
        env!("CARGO_MANIFEST_DIR")
    );
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

async fn install(app: &Router) {
    for (id, name) in [
        ("nl.vng.gesprekken", "gesprekken"),
        ("nl.example.notities", "notities"),
        ("nl.vng.meldingen", "meldingen"),
        ("nl.vng.vergunningen", "vergunningen"),
    ] {
        let (s, _, _) = call(
            app,
            "PUT",
            &format!("/profiles/{id}"),
            Some(ADMIN),
            Some(profile(name)),
        )
        .await;
        assert_eq!(s, StatusCode::OK);
    }
}

fn ev(id: &str, ty: &str, subject: &str, data: Value) -> Value {
    json!({ "specversion": "1.0", "id": id, "source": "urn:test", "type": ty, "subject": subject, "data": data })
}

fn enc(s: &str) -> String {
    s.replace(':', "%3A").replace('/', "%2F")
}

const G1: &str = "urn:uuid:gesprek-1";

async fn start_gesprek(app: &Router) {
    let (s, b, _) = call(
        app,
        "POST",
        "/events",
        Some(SUZANNE),
        Some(ev(
            "e1",
            "nl.vng.gesprekken.gesprek.gestart",
            G1,
            json!({
                "onderwerp": "Vraag over mijn aanvraag",
                "team": "wmo",
                "aanleiding": { "zaak": "https://zaken.example.nl/zaken/1" },
                "initiatiefnemer": { "partij": SUZANNE_ID, "rol": "inwoner" }
            }),
        )),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{b}");
}

#[tokio::test]
async fn profiles_need_an_admin() {
    let app = app();
    let (s, _, _) = call(
        &app,
        "PUT",
        "/profiles/nl.vng.gesprekken",
        Some(SUZANNE),
        Some(profile("gesprekken")),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, _, _) = call(
        &app,
        "PUT",
        "/profiles/nl.vng.gesprekken",
        None,
        Some(profile("gesprekken")),
    )
    .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    install(&app).await;
    let (_, b, _) = call(&app, "GET", "/profiles", None, None).await;
    assert_eq!(b["profiles"].as_array().unwrap().len(), 4);
}

#[tokio::test]
async fn read_rules_decide_who_sees_a_gesprek() {
    let app = app();
    install(&app).await;
    start_gesprek(&app).await;

    let uri = format!("/subjects/{}", enc(G1));
    let (s, b, _) = call(&app, "GET", &uri, Some(SUZANNE), None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(b["status"], "open");
    assert_eq!(b["rollen"]["inwoner"][0], SUZANNE_ID);
    // A stranger gets the same answer as for something that does not exist.
    let (s, _, _) = call(&app, "GET", &uri, Some(KAREL), None).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    // A member of the team sees it via the claim.
    let (s, _, _) = call(&app, "GET", &uri, Some(MERVE), None).await;
    assert_eq!(s, StatusCode::OK);
}

#[tokio::test]
async fn write_rules_schema_and_projection() {
    let app = app();
    install(&app).await;
    start_gesprek(&app).await;

    let bijdrage = |id: &str, tekst: Value| {
        ev(
            id,
            "nl.vng.gesprekken.bijdrage.toegevoegd",
            G1,
            json!({ "tekst": tekst }),
        )
    };
    let (s, _, _) = call(
        &app,
        "POST",
        "/events",
        Some(SUZANNE),
        Some(bijdrage("e2", json!("Hallo"))),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED);
    let (s, _, _) = call(
        &app,
        "POST",
        "/events",
        Some(KAREL),
        Some(bijdrage("e3", json!("Ik ook"))),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, b, _) = call(
        &app,
        "POST",
        "/events",
        Some(SUZANNE),
        Some(bijdrage("e4", json!(42))),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(b["invalidParams"][0]["code"], "schema");

    // Unknown types and missing fields are rejected.
    let (s, _, _) = call(
        &app,
        "POST",
        "/events",
        Some(SUZANNE),
        Some(ev("e5", "nl.vng.onbekend", G1, json!({}))),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);

    // After closing, the inwoner can no longer add (append rule checks status).
    let (s, _, _) = call(
        &app,
        "POST",
        "/events",
        Some(SUZANNE),
        Some(ev(
            "e6",
            "nl.vng.gesprekken.gesprek.afgesloten",
            G1,
            json!({ "reden": "beantwoord" }),
        )),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED);
    let (s, _, _) = call(
        &app,
        "POST",
        "/events",
        Some(SUZANNE),
        Some(bijdrage("e7", json!("Nog iets"))),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);

    let (_, b, _) = call(
        &app,
        "GET",
        &format!("/subjects/{}", enc(G1)),
        Some(SUZANNE),
        None,
    )
    .await;
    assert_eq!(b["aantalBijdragen"], 1);
    assert_eq!(b["bijdragen"][0]["tekst"], "Hallo");
    assert_eq!(b["status"], "afgesloten");
    // Gesprekken only accept their own event types, not generic commits.
    let commit = json!({ "specversion": "1.0", "id": "c1", "source": "urn:test", "type": "json.commit", "subject": G1,
        "data": { "resourceId": G1, "patch": { "status": "open" } } });
    let (s, b, _) = call(&app, "POST", "/events", Some(SUZANNE), Some(commit)).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{b}");
}

#[tokio::test]
async fn idempotency_and_previous_commit() {
    let app = app();
    install(&app).await;
    start_gesprek(&app).await;
    let e = ev(
        "e2",
        "nl.vng.gesprekken.bijdrage.toegevoegd",
        G1,
        json!({ "tekst": "Hallo" }),
    );
    let (s, first, _) = call(&app, "POST", "/events", Some(SUZANNE), Some(e.clone())).await;
    assert_eq!(s, StatusCode::CREATED);
    let (s, again, h) = call(&app, "POST", "/events", Some(SUZANNE), Some(e.clone())).await;
    assert_eq!(s, StatusCode::CREATED);
    assert_eq!(first, again);
    assert_eq!(h.get("idempotent-replayed").unwrap(), "true");
    // Same id, other body: conflict. Same id, other caller: conflict, without the response.
    let other = ev(
        "e2",
        "nl.vng.gesprekken.bijdrage.toegevoegd",
        G1,
        json!({ "tekst": "Anders" }),
    );
    let (s, _, _) = call(&app, "POST", "/events", Some(SUZANNE), Some(other)).await;
    assert_eq!(s, StatusCode::CONFLICT);
    let (s, b, _) = call(&app, "POST", "/events", Some(MERVE), Some(e)).await;
    assert_eq!(s, StatusCode::CONFLICT);
    assert!(b.get("events").is_none());

    let mut stale = ev(
        "e3",
        "nl.vng.gesprekken.bijdrage.toegevoegd",
        G1,
        json!({ "tekst": "Oud" }),
    );
    stale["previouscommit"] = json!("e1");
    let (s, _, _) = call(&app, "POST", "/events", Some(SUZANNE), Some(stale)).await;
    assert_eq!(s, StatusCode::PRECONDITION_FAILED);
    let mut fresh = ev(
        "e4",
        "nl.vng.gesprekken.bijdrage.toegevoegd",
        G1,
        json!({ "tekst": "Nieuw" }),
    );
    fresh["previouscommit"] = json!("e2");
    let (s, _, _) = call(&app, "POST", "/events", Some(SUZANNE), Some(fresh)).await;
    assert_eq!(s, StatusCode::CREATED);
}

fn tx(id: &str, commits: Value) -> Value {
    json!({ "specversion": "1.0", "id": id, "source": "urn:test", "type": "json.commit.transaction", "subject": "urn:notities", "data": { "commits": commits } })
}

#[tokio::test]
async fn json_commit_transactions_are_all_or_nothing() {
    let app = app();
    install(&app).await;
    let n1 = json!({ "resourceId": "urn:n1", "type": "nl.example.notitie", "resourceData": { "titel": "Een", "eigenaar": SUZANNE_ID, "label": "werk" } });
    let bad = json!({ "resourceId": "urn:n2", "type": "nl.example.notitie", "resourceData": { "titel": "Zonder eigenaar" } });
    let (s, _, _) = call(
        &app,
        "POST",
        "/events",
        Some(SUZANNE),
        Some(tx("t1", json!([n1, bad]))),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    let (s, _, _) = call(&app, "GET", "/subjects/urn%3An1", Some(SUZANNE), None).await;
    assert_eq!(
        s,
        StatusCode::NOT_FOUND,
        "nothing of a rejected transaction is stored"
    );

    let n2 = json!({ "resourceId": "urn:n2", "schema": "https://example.nl/schemas/nl.example.notitie", "resourceData": { "titel": "Twee", "eigenaar": SUZANNE_ID, "label": "thuis", "lezers": ["https://id.example.nl/karel"] } });
    let (s, b, _) = call(
        &app,
        "POST",
        "/events",
        Some(SUZANNE),
        Some(tx("t2", json!([n1, n2]))),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{b}");
    assert_eq!(b["events"].as_array().unwrap().len(), 2);
    assert_eq!(b["events"][0]["id"], "t2:0");

    // Patch with previousCommit, by the owner.
    let patch = json!({ "resourceId": "urn:n1", "patch": { "tekst": "bijgewerkt" }, "previousCommit": "t2:0" });
    let (s, _, _) = call(
        &app,
        "POST",
        "/events",
        Some(SUZANNE),
        Some(tx("t3", json!([patch.clone()]))),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED);
    let (s, _, _) = call(
        &app,
        "POST",
        "/events",
        Some(SUZANNE),
        Some(tx("t4", json!([patch]))),
    )
    .await;
    assert_eq!(s, StatusCode::PRECONDITION_FAILED);
    // A reader may not write.
    let (s, _, _) = call(
        &app,
        "POST",
        "/events",
        Some(KAREL),
        Some(tx(
            "t5",
            json!([{ "resourceId": "urn:n2", "patch": { "titel": "Van Karel" } }]),
        )),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    // Creating something you would not own is not allowed either.
    let (s, _, _) = call(&app, "POST", "/events", Some(KAREL), Some(tx("t6", json!([{ "resourceId": "urn:n3", "type": "nl.example.notitie", "resourceData": { "titel": "x", "eigenaar": SUZANNE_ID } }])))).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn query_is_filter_and_read_rule() {
    let app = app();
    install(&app).await;
    let mk = |id: &str, label: &str, eigenaar: &str| json!({ "resourceId": id, "type": "nl.example.notitie", "resourceData": { "titel": id, "eigenaar": eigenaar, "label": label } });
    let karel = "https://id.example.nl/karel";
    call(
        &app,
        "POST",
        "/events",
        Some(SUZANNE),
        Some(tx(
            "t1",
            json!([
                mk("urn:a", "werk", SUZANNE_ID),
                mk("urn:b", "thuis", SUZANNE_ID),
                mk("urn:c", "werk", SUZANNE_ID)
            ]),
        )),
    )
    .await;
    call(
        &app,
        "POST",
        "/events",
        Some(KAREL),
        Some(tx("t2", json!([mk("urn:d", "werk", karel)]))),
    )
    .await;

    let q = |filter: Value| json!({ "type": "nl.example.notitie", "filter": filter, "fields": ["titel"] });
    let (s, b, _) = call(
        &app,
        "POST",
        "/query",
        Some(SUZANNE),
        Some(q(json!("label = 'werk'"))),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{b}");
    let titles: Vec<_> = b["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["titel"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        titles,
        vec!["urn:a", "urn:c"],
        "Karel's note is not visible to Suzanne"
    );
    assert!(
        b["results"][0].get("label").is_none(),
        "fields picks fields"
    );
    assert!(b["feedCursor"].as_str().unwrap().parse::<u64>().unwrap() >= 4);

    // CQL2-JSON works too, and paging.
    let (_, b, _) = call(&app, "POST", "/query", Some(SUZANNE), Some(json!({ "type": "nl.example.notitie", "filter": { "op": "in", "args": [ { "property": "label" }, ["werk", "thuis"] ] }, "pageSize": 2 }))).await;
    assert_eq!(b["results"].as_array().unwrap().len(), 2);
    let next = b["next"].as_str().unwrap().to_string();
    let (_, b, _) = call(
        &app,
        "POST",
        "/query",
        Some(SUZANNE),
        Some(json!({ "type": "nl.example.notitie", "pageSize": 2, "cursor": next })),
    )
    .await;
    assert_eq!(b["results"].as_array().unwrap().len(), 1);
    assert!(b["next"].is_null());

    // Clients may not use actor(), nor fields that are not queryable.
    let (s, _, _) = call(
        &app,
        "POST",
        "/query",
        Some(SUZANNE),
        Some(q(json!("eigenaar = actor()"))),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    let (s, _, _) = call(
        &app,
        "POST",
        "/query",
        Some(SUZANNE),
        Some(q(json!("tekst = 'x'"))),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn event_listing_and_timeline_follow_read_rules() {
    let app = app();
    install(&app).await;
    start_gesprek(&app).await;
    call(
        &app,
        "POST",
        "/events",
        Some(SUZANNE),
        Some(ev(
            "e2",
            "nl.vng.gesprekken.bijdrage.toegevoegd",
            G1,
            json!({ "tekst": "Hallo" }),
        )),
    )
    .await;

    let (_, b, _) = call(&app, "GET", "/events?format=json", Some(SUZANNE), None).await;
    assert_eq!(b["events"].as_array().unwrap().len(), 2);
    assert_eq!(b["events"][1]["sequence"], "2");
    assert_eq!(b["events"][1]["actor"], SUZANNE_ID);
    let (_, b, _) = call(&app, "GET", "/events?format=json", Some(KAREL), None).await;
    assert_eq!(b["events"].as_array().unwrap().len(), 0);
    let (_, b, _) = call(
        &app,
        "GET",
        "/events?format=json&after=1&type=nl.vng.gesprekken.bijdrage",
        Some(MERVE),
        None,
    )
    .await;
    assert_eq!(b["events"].as_array().unwrap().len(), 1);

    let (s, b, _) = call(
        &app,
        "GET",
        &format!("/subjects/{}/events", enc(G1)),
        Some(SUZANNE),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(b["events"].as_array().unwrap().len(), 2);
    let (s, _, _) = call(
        &app,
        "GET",
        &format!("/subjects/{}/events", enc(G1)),
        Some(KAREL),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

async fn sse_until(
    app: &Router,
    uri: &str,
    headers: &[(&str, &str)],
    wanted: usize,
    during: impl std::future::Future<Output = ()>,
) -> Vec<Value> {
    let mut req = Request::builder()
        .uri(uri)
        .header("accept", "text/event-stream");
    for (k, v) in headers {
        req = req.header(*k, *v);
    }
    let res = app
        .clone()
        .oneshot(req.body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let mut body = res.into_body();
    during.await;
    let mut buf = String::new();
    let mut events = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    while events.len() < wanted {
        let frame = tokio::time::timeout_at(deadline, body.frame())
            .await
            .expect("timed out waiting for SSE")
            .unwrap()
            .unwrap();
        if let Ok(data) = frame.into_data() {
            buf.push_str(&String::from_utf8_lossy(&data));
            while let Some(end) = buf.find("\n\n") {
                let chunk = buf[..end].to_string();
                buf = buf[end + 2..].to_string();
                for line in chunk.lines() {
                    if let Some(d) = line
                        .strip_prefix("data: ")
                        .or_else(|| line.strip_prefix("data:"))
                    {
                        if let Ok(v) = serde_json::from_str::<Value>(d) {
                            events.push(v);
                        }
                    }
                }
            }
        }
    }
    events
}

#[tokio::test]
async fn sse_catches_up_from_last_event_id_then_goes_live() {
    let app = app();
    install(&app).await;
    start_gesprek(&app).await;
    let app2 = app.clone();
    let live = async move {
        // A stranger's event elsewhere must not show up; Suzanne's next bijdrage must.
        call(&app2, "POST", "/events", Some(KAREL), Some(tx("t1", json!([{ "resourceId": "urn:k", "type": "nl.example.notitie", "resourceData": { "titel": "k", "eigenaar": "https://id.example.nl/karel" } }])))).await;
        call(
            &app2,
            "POST",
            "/events",
            Some(SUZANNE),
            Some(ev(
                "e2",
                "nl.vng.gesprekken.bijdrage.toegevoegd",
                G1,
                json!({ "tekst": "Live" }),
            )),
        )
        .await;
    };
    let auth = format!("Bearer {SUZANNE}");
    let events = sse_until(
        &app,
        "/events",
        &[("authorization", &auth), ("last-event-id", "0")],
        2,
        live,
    )
    .await;
    assert_eq!(events[0]["id"], "e1");
    assert_eq!(events[1]["id"], "e2");
    assert_eq!(events[1]["data"]["tekst"], "Live");
}

#[tokio::test]
async fn tickets_keep_tokens_out_of_urls() {
    let app = app();
    install(&app).await;
    start_gesprek(&app).await;
    let (s, b, _) = call(&app, "POST", "/events/tickets", Some(SUZANNE), None).await;
    assert_eq!(s, StatusCode::OK);
    let ticket = b["ticket"].as_str().unwrap().to_string();
    let events = sse_until(
        &app,
        &format!("/events?ticket={ticket}&after=0"),
        &[],
        1,
        async {},
    )
    .await;
    assert_eq!(events[0]["subject"], G1);
    // A ticket works once.
    let req = Request::builder()
        .uri(format!("/events?ticket={ticket}"))
        .header("accept", "text/event-stream")
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        app.clone().oneshot(req).await.unwrap().status(),
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn jwt_tokens_work() {
    let app = app();
    install(&app).await;
    let claims = json!({ "sub": SUZANNE_ID, "exp": 4_000_000_000u64 });
    let token = jsonwebtoken::encode(
        &jsonwebtoken::Header::default(),
        &claims,
        &jsonwebtoken::EncodingKey::from_secret(b"test-secret"),
    )
    .unwrap();
    start_gesprek(&app).await;
    let (s, _, _) = call(
        &app,
        "GET",
        &format!("/subjects/{}", enc(G1)),
        Some(&token),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let bad = jsonwebtoken::encode(
        &jsonwebtoken::Header::default(),
        &claims,
        &jsonwebtoken::EncodingKey::from_secret(b"wrong"),
    )
    .unwrap();
    let (s, _, _) = call(
        &app,
        "GET",
        &format!("/subjects/{}", enc(G1)),
        Some(&bad),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn everything_survives_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("oe.redb");
    {
        let app = app_with(Arc::new(RedbStore::open(&path).unwrap()));
        install(&app).await;
        start_gesprek(&app).await;
    }
    let app = app_with(Arc::new(RedbStore::open(&path).unwrap()));
    let (s, b, _) = call(
        &app,
        "GET",
        &format!("/subjects/{}", enc(G1)),
        Some(SUZANNE),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(b["onderwerp"], "Vraag over mijn aanvraag");
    let (s, _, _) = call(
        &app,
        "POST",
        "/events",
        Some(SUZANNE),
        Some(ev(
            "e2",
            "nl.vng.gesprekken.bijdrage.toegevoegd",
            G1,
            json!({ "tekst": "Na herstart" }),
        )),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED);
    let (_, b, _) = call(&app, "GET", "/events?format=json", Some(SUZANNE), None).await;
    assert_eq!(b["events"][1]["sequence"], "2");
}

#[tokio::test]
async fn optional_fields_may_be_left_out() {
    let app = app();
    install(&app).await;
    let (s, b, _) = call(
        &app,
        "POST",
        "/events",
        Some(SUZANNE),
        Some(ev(
            "e1",
            "nl.vng.gesprekken.gesprek.gestart",
            G1,
            json!({
                "onderwerp": "Kort", "initiatiefnemer": { "partij": SUZANNE_ID, "rol": "inwoner" }
            }),
        )),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{b}");
    let (_, b, _) = call(
        &app,
        "GET",
        &format!("/subjects/{}", enc(G1)),
        Some(SUZANNE),
        None,
    )
    .await;
    assert!(b.get("team").is_none() && b.get("aanleiding").is_none());
}

const KIM: &str = "dev:https://id.example.nl/kim|rol=kcc";

#[tokio::test]
async fn field_rules_limit_what_kcc_sees() {
    let app = app();
    install(&app).await;
    start_gesprek(&app).await;
    call(
        &app,
        "POST",
        "/events",
        Some(SUZANNE),
        Some(ev(
            "e2",
            "nl.vng.gesprekken.bijdrage.toegevoegd",
            G1,
            json!({ "tekst": "Geheim" }),
        )),
    )
    .await;

    let (s, b, _) = call(
        &app,
        "GET",
        &format!("/subjects/{}", enc(G1)),
        Some(KIM),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(b["onderwerp"], "Vraag over mijn aanvraag");
    assert!(b.get("bijdragen").is_none() && b.get("deelnames").is_none());
    assert_eq!(b["_meta"]["limited"], true);

    let (_, b, _) = call(&app, "GET", "/events?format=json", Some(KIM), None).await;
    assert_eq!(b["events"].as_array().unwrap().len(), 2);
    assert!(b["events"][1].get("data").is_none());
    assert_eq!(b["events"][1]["dataHidden"], true);

    let (_, b, _) = call(
        &app,
        "POST",
        "/query",
        Some(KIM),
        Some(json!({ "type": "nl.vng.gesprekken.gesprek", "filter": "status = 'open'" })),
    )
    .await;
    assert_eq!(b["results"].as_array().unwrap().len(), 1);
    assert!(b["results"][0].get("bijdragen").is_none());
    // Filtering on a field KCC may not see is the same error as an unknown field.
    let (s, _, _) = call(
        &app,
        "POST",
        "/query",
        Some(KIM),
        Some(json!({ "type": "nl.vng.gesprekken.gesprek", "filter": "deelnames.partij = 'x'" })),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    // Suzanne still sees everything.
    let (_, b, _) = call(
        &app,
        "GET",
        &format!("/subjects/{}", enc(G1)),
        Some(SUZANNE),
        None,
    )
    .await;
    assert_eq!(b["bijdragen"][0]["tekst"], "Geheim");
}

#[tokio::test]
async fn ui_and_personas() {
    let app = app();
    install(&app).await;
    let (s, b, _) = call(&app, "GET", "/ui", None, None).await;
    assert_eq!(s, StatusCode::OK);
    assert!(b.as_str().unwrap().contains("OpenEvents"));
    let (_, b, _) = call(&app, "GET", "/ui/config", None, None).await;
    assert_eq!(b["devAuth"], true);
    let labels: Vec<_> = b["personas"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["label"].as_str().unwrap().to_string())
        .collect();
    assert!(labels.iter().any(|l| l.contains("KCC")));
    assert!(labels[0].contains("Beheerder"));
}

/// A field rule that depends on the subject: other inwoners see a melding without the
/// melder, and cannot find out who made it by filtering on the hidden field.
#[tokio::test]
async fn filters_only_see_shown_fields() {
    let app = app();
    install(&app).await;
    let (s, b, _) = call(
        &app,
        "POST",
        "/events",
        Some("dev:https://id.example.nl/suzanne|rol=inwoner"),
        Some(ev(
            "m1",
            "nl.vng.meldingen.melding.gedaan",
            "urn:uuid:melding-1",
            json!({
                "zaaknummer": "MOR-1", "categorie": "Afval", "omschrijving": "Volle container", "team": "afval",
                "locatie": { "type": "Point", "coordinates": [5.12, 52.09] },
                "melder": { "partij": SUZANNE_ID, "rol": "inwoner" }
            }),
        )),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{b}");
    let karel = "dev:https://id.example.nl/karel|rol=inwoner";
    let q = |filter: &str| json!({ "type": "nl.vng.meldingen.melding", "filter": filter });
    let (_, b, _) = call(
        &app,
        "POST",
        "/query",
        Some(karel),
        Some(q("categorie = 'Afval'")),
    )
    .await;
    assert_eq!(b["results"].as_array().unwrap().len(), 1);
    assert!(b["results"][0].get("melder").is_none());
    let probe = format!("melder.partij = '{SUZANNE_ID}'");
    let (_, b, _) = call(&app, "POST", "/query", Some(karel), Some(q(&probe))).await;
    assert_eq!(b["results"].as_array().unwrap().len(), 0);
    // Suzanne finds her own melding with the same filter.
    let (_, b, _) = call(
        &app,
        "POST",
        "/query",
        Some("dev:https://id.example.nl/suzanne|rol=inwoner"),
        Some(q(&probe)),
    )
    .await;
    assert_eq!(b["results"].as_array().unwrap().len(), 1);
}

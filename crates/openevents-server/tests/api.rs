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
        ..Default::default()
    };
    router(AppState {
        register,
        auth,
        tickets: Arc::new(Tickets::default()),
        schemas: Arc::new(openevents_server::schemas::Schemas::new(Some(
            concat!(env!("CARGO_MANIFEST_DIR"), "/../../examples/schemas").into(),
        ))),
        notificaties: None,
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
        ("nl.vng.taken", "taken"),
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
            json!({ "type": "nl.vng.gesprekken.gesprek", "resourceData": {
                "gespreksonderwerp": "Vraag over mijn aanvraag",
                "team": "wmo",
                "aanleidingZaak": "https://zaken.example.nl/zaken/1",
                "gespreksdeelnames": [ { "type": "partij", "partij": SUZANNE_ID, "naam": "Suzanne" } ]
            } }),
        )),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{b}");
}

/// A bijdrage is its own resource that points at its gesprek (ADR-0008).
fn bijdrage(id: &str, subject: &str, tekst: Value) -> Value {
    ev(
        id,
        "nl.vng.gesprekken.bijdrage.toegevoegd",
        subject,
        json!({ "resourceData": { "gesprek": G1, "tekstGespreksBijdrage": tekst } }),
    )
}

const B1: &str = "urn:uuid:bijdrage-1";

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
    assert_eq!(b["profiles"].as_array().unwrap().len(), 5);
}

#[tokio::test]
async fn read_rules_decide_who_sees_a_gesprek() {
    let app = app();
    install(&app).await;
    start_gesprek(&app).await;

    let uri = format!("/subjects/{}", enc(G1));
    let (s, b, _) = call(&app, "GET", &uri, Some(SUZANNE), None).await;
    assert_eq!(s, StatusCode::OK);
    assert!(b["momentEindeGesprek"].is_null());
    assert_eq!(b["gespreksdeelnames"][0]["partij"], SUZANNE_ID);
    // `set` in the action: the register fills in when it started.
    assert!(b["aanvangsmomentGesprek"].is_string());
    // The shape of a gesprek comes from the VNG API Lab, inlined when registered.
    let (_, p, _) = call(&app, "GET", "/profiles/nl.vng.gesprekken", None, None).await;
    let t = &p["types"]["nl.vng.gesprekken.gesprek"];
    assert!(t["schemaSources"][0]
        .as_str()
        .unwrap()
        .ends_with("/schemas/gesprekken/next.json"));
    assert_eq!(t["schema"]["allOf"][0]["required"][0], "gespreksonderwerp");
    // A stranger gets the same answer as for something that does not exist.
    let (s, _, _) = call(&app, "GET", &uri, Some(KAREL), None).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    // A member of the team sees it via the claim.
    let (s, _, _) = call(&app, "GET", &uri, Some(MERVE), None).await;
    assert_eq!(s, StatusCode::OK);
}

#[tokio::test]
async fn actions_decide_what_a_commit_may_do() {
    let app = app();
    install(&app).await;
    start_gesprek(&app).await;

    let post = |token: &'static str, e: Value| {
        let app = app.clone();
        async move { call(&app, "POST", "/events", Some(token), Some(e)).await }
    };
    let (s, b, _) = post(SUZANNE, bijdrage("e2", B1, json!("Hallo"))).await;
    assert_eq!(s, StatusCode::CREATED, "{b}");
    // Karel is not in the gesprek: the rule follows `gesprek` to check the deelnames.
    let (s, _, _) = post(KAREL, bijdrage("e3", "urn:uuid:b-karel", json!("Ik ook"))).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, b, _) = post(SUZANNE, bijdrage("e4", "urn:uuid:b-4", json!(42))).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(b["invalidParams"][0]["code"], "schema");
    // The server sets `verzender`; the client may not.
    let (s, b, _) = post(SUZANNE, ev("e4b", "nl.vng.gesprekken.bijdrage.toegevoegd", "urn:uuid:b-4b",
        json!({ "resourceData": { "gesprek": G1, "tekstGespreksBijdrage": "x", "verzender": "https://id.example.nl/iemand" } }))).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(b["invalidParams"][0]["code"], "field-not-allowed");
    // An action only allows the commits it lists: no patching a gesprek with a bijdrage.
    let (s, b, _) = post(
        SUZANNE,
        ev(
            "e4c",
            "nl.vng.gesprekken.bijdrage.toegevoegd",
            G1,
            json!({ "patch": { "afsluitreden": "klaar" } }),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(b["invalidParams"][0]["code"], "not-allowed");
    // A create of something that exists is refused.
    let (s, b, _) = post(SUZANNE, bijdrage("e4d", B1, json!("Nog eens"))).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(b["invalidParams"][0]["code"], "exists");

    // Unknown types are rejected.
    let (s, _, _) = post(SUZANNE, ev("e5", "nl.vng.onbekend", G1, json!({}))).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);

    // Closing sets when it ended; after that no more bijdragen.
    let (s, b, _) = post(
        SUZANNE,
        ev(
            "e6",
            "nl.vng.gesprekken.gesprek.afgesloten",
            G1,
            json!({ "patch": { "afsluitreden": "beantwoord" } }),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{b}");
    let (s, _, _) = post(SUZANNE, bijdrage("e7", "urn:uuid:b-7", json!("Nog iets"))).await;
    assert_eq!(s, StatusCode::FORBIDDEN);

    let (_, b, _) = call(
        &app,
        "GET",
        &format!("/subjects/{}", enc(G1)),
        Some(SUZANNE),
        None,
    )
    .await;
    assert!(b["momentEindeGesprek"].is_string());
    assert_eq!(b["afsluitreden"], "beantwoord");
    // What used to be a projection (bijdragen, aantal) is a query.
    let (s, b, _) = call(
        &app,
        "POST",
        "/query",
        Some(SUZANNE),
        Some(
            json!({ "type": "nl.vng.gesprekken.bijdrage", "filter": format!("gesprek = '{G1}'") }),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{b}");
    assert_eq!(b["results"].as_array().unwrap().len(), 1);
    assert_eq!(b["results"][0]["tekstGespreksBijdrage"], "Hallo");
    assert_eq!(b["results"][0]["verzender"], SUZANNE_ID);
    // The event in the log: the action as type, the bijdrage as subject, the commit as data.
    let (_, b, _) = call(
        &app,
        "GET",
        &format!("/subjects/{}/events", enc(B1)),
        Some(SUZANNE),
        None,
    )
    .await;
    let e = &b["events"][0];
    assert_eq!(e["type"], "nl.vng.gesprekken.bijdrage.toegevoegd");
    assert_eq!(e["data"]["type"], "nl.vng.gesprekken.bijdrage");
    assert_eq!(e["data"]["resourceData"]["verzender"], SUZANNE_ID);
    // Gesprekken only accept their actions, not bare commits.
    let commit = json!({ "specversion": "1.0", "id": "c1", "source": "urn:test", "type": "json.commit", "subject": G1,
        "data": { "resourceId": G1, "patch": { "momentEindeGesprek": null } } });
    let (s, b, _) = post(SUZANNE, commit).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{b}");
    assert_eq!(b["invalidParams"][0]["code"], "commits-not-allowed");
}

/// One action, two commits: start a gesprek with its first bijdrage, all or nothing.
/// Then filters that follow the reference from bijdrage to gesprek.
#[tokio::test]
async fn one_action_many_commits_and_references() {
    let app = app();
    install(&app).await;
    let g = "urn:uuid:gesprek-2";
    let start = |id: &str, tekst: Value| {
        ev(
            id,
            "nl.vng.gesprekken.gesprek.gestart",
            g,
            json!({ "commits": [
        { "resourceId": g, "type": "nl.vng.gesprekken.gesprek", "resourceData": {
            "gespreksonderwerp": "Traplift", "team": "wmo", "gespreksdeelnames": [ { "type": "partij", "partij": SUZANNE_ID, "naam": "Suzanne" } ] } },
        { "resourceId": "urn:uuid:b-eerste", "type": "nl.vng.gesprekken.bijdrage",
          "resourceData": { "gesprek": g, "tekstGespreksBijdrage": tekst } }
    ] }),
        )
    };
    let (s, _, _) = call(
        &app,
        "POST",
        "/events",
        Some(SUZANNE),
        Some(start("s1", json!(1))),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    let (s, _, _) = call(
        &app,
        "GET",
        &format!("/subjects/{}", enc(g)),
        Some(SUZANNE),
        None,
    )
    .await;
    assert_eq!(
        s,
        StatusCode::NOT_FOUND,
        "nothing of a rejected action is stored"
    );
    // The bijdrage's rule sees the gesprek created earlier in the same request.
    let (s, b, _) = call(
        &app,
        "POST",
        "/events",
        Some(SUZANNE),
        Some(start("s2", json!("Hoe vraag ik een traplift aan?"))),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{b}");
    assert_eq!(b["events"][1]["id"], "s2:1");

    let q = |filter: &str| json!({ "type": "nl.vng.gesprekken.bijdrage", "filter": filter });
    let (s, b, _) = call(
        &app,
        "POST",
        "/query",
        Some(MERVE),
        Some(q(
            "gesprek.gespreksonderwerp = 'Traplift' AND gesprek.momentEindeGesprek IS NULL",
        )),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{b}");
    assert_eq!(b["results"].as_array().unwrap().len(), 1);
    let (_, b, _) = call(
        &app,
        "POST",
        "/query",
        Some(KAREL),
        Some(q("gesprek.gespreksonderwerp = 'Traplift'")),
    )
    .await;
    assert_eq!(b["results"].as_array().unwrap().len(), 0);
    // Not queryable: a field of the bijdrage that is not listed.
    let (s, _, _) = call(
        &app,
        "POST",
        "/query",
        Some(MERVE),
        Some(q("tekstGespreksBijdrage = 'x'")),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn idempotency_and_previous_commit() {
    let app = app();
    install(&app).await;
    start_gesprek(&app).await;
    let e = bijdrage("e2", B1, json!("Hallo"));
    let (s, first, _) = call(&app, "POST", "/events", Some(SUZANNE), Some(e.clone())).await;
    assert_eq!(s, StatusCode::CREATED);
    let (s, again, h) = call(&app, "POST", "/events", Some(SUZANNE), Some(e.clone())).await;
    assert_eq!(s, StatusCode::CREATED);
    assert_eq!(first, again);
    assert_eq!(h.get("idempotent-replayed").unwrap(), "true");
    // Same id, other body: conflict. Same id, other caller: conflict, without the response.
    let other = bijdrage("e2", B1, json!("Anders"));
    let (s, _, _) = call(&app, "POST", "/events", Some(SUZANNE), Some(other)).await;
    assert_eq!(s, StatusCode::CONFLICT);
    let (s, b, _) = call(&app, "POST", "/events", Some(MERVE), Some(e)).await;
    assert_eq!(s, StatusCode::CONFLICT);
    assert!(b.get("events").is_none());

    let sluit = |id: &str, prev: &str| {
        ev(
            id,
            "nl.vng.gesprekken.gesprek.afgesloten",
            G1,
            json!({ "patch": {}, "previousCommit": prev }),
        )
    };
    let stale = sluit("e3", "e0");
    let (s, _, _) = call(&app, "POST", "/events", Some(SUZANNE), Some(stale)).await;
    assert_eq!(s, StatusCode::PRECONDITION_FAILED);
    let fresh = sluit("e4", "e1");
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
        Some(bijdrage("e2", B1, json!("Hallo"))),
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
    // The bijdrage is its own resource with its own timeline.
    assert_eq!(b["events"].as_array().unwrap().len(), 1);
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
            Some(bijdrage("e2", B1, json!("Live"))),
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
    assert_eq!(
        events[1]["data"]["resourceData"]["tekstGespreksBijdrage"],
        "Live"
    );
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
    assert_eq!(b["gespreksonderwerp"], "Vraag over mijn aanvraag");
    let (s, _, _) = call(
        &app,
        "POST",
        "/events",
        Some(SUZANNE),
        Some(bijdrage("e2", B1, json!("Na herstart"))),
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
            json!({ "type": "nl.vng.gesprekken.gesprek", "resourceData": {
                "gespreksonderwerp": "Kort", "gespreksdeelnames": [ { "type": "partij", "partij": SUZANNE_ID, "naam": "Suzanne" } ]
            } }),
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
    assert!(b.get("team").is_none() && b.get("aanleidingZaak").is_none());
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
        Some(bijdrage("e2", B1, json!("Geheim"))),
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
    assert_eq!(b["gespreksonderwerp"], "Vraag over mijn aanvraag");
    assert!(b.get("gespreksdeelnames").is_none());
    assert_eq!(b["_meta"]["limited"], true);
    // The bijdrage itself is not for the KCC at all.
    let (s, _, _) = call(
        &app,
        "GET",
        &format!("/subjects/{}", enc(B1)),
        Some(KIM),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);

    let (_, b, _) = call(&app, "GET", "/events?format=json", Some(KIM), None).await;
    assert_eq!(b["events"].as_array().unwrap().len(), 1);
    assert!(b["events"][0].get("data").is_none());
    assert_eq!(b["events"][0]["dataHidden"], true);

    let (_, b, _) = call(
        &app,
        "POST",
        "/query",
        Some(KIM),
        Some(
            json!({ "type": "nl.vng.gesprekken.gesprek", "filter": "momentEindeGesprek IS NULL" }),
        ),
    )
    .await;
    assert_eq!(b["results"].as_array().unwrap().len(), 1);
    assert!(b["results"][0].get("gespreksdeelnames").is_none());
    // Filtering on a field KCC may not see is the same error as an unknown field.
    let (s, _, _) = call(
        &app,
        "POST",
        "/query",
        Some(KIM),
        Some(json!({ "type": "nl.vng.gesprekken.gesprek", "filter": "gespreksdeelnames.partij = 'x'" })),
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
    assert_eq!(b["gespreksdeelnames"][0]["partij"], SUZANNE_ID);
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
            json!({ "resourceData": {
                "zaaknummer": "MOR-1", "categorie": "Afval", "omschrijving": "Volle container", "team": "afval",
                "locatie": { "type": "Point", "coordinates": [5.12, 52.09] },
                "melder": { "partij": SUZANNE_ID, "rol": "inwoner" }
            } }),
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

/// A batch: every event on its own, with the outcome per event; then the OpenAPI.
#[tokio::test]
async fn batch_and_openapi() {
    let app = app();
    install(&app).await;
    let systeem = "dev:https://id.example.nl/signalen|rol=systeem";
    let melding = |id: &str, nr: &str| {
        json!({
            "specversion": "1.0", "id": id, "source": "https://signalen.demodam.nl",
            "type": "nl.vng.meldingen.melding.gedaan", "subject": format!("https://zaken.demodam.nl/meldingen/{nr}"),
            "data": { "resourceData": {
                "zaaknummer": nr, "categorie": "Afval", "omschrijving": "Volle container", "team": "afval",
                "locatie": { "type": "Point", "coordinates": [5.12, 52.09] },
                "melder": { "partij": SUZANNE_ID, "rol": "inwoner" }
            } }
        })
    };
    let mut kapot = melding("b3", "MOR-3");
    kapot["data"]["resourceData"]["locatie"] = json!("ergens");
    let batch = json!([melding("b1", "MOR-1"), melding("b2", "MOR-2"), kapot]);
    let req = Request::builder()
        .method("POST")
        .uri("/events")
        .header("authorization", format!("Bearer {systeem}"))
        .header("content-type", "application/cloudevents-batch+json")
        .body(Body::from(batch.to_string()))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::MULTI_STATUS);
    let b: Value =
        serde_json::from_slice(&res.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(b["stored"], 2);
    assert_eq!(b["results"][2]["status"], 422);
    assert_eq!(
        b["results"][2]["problem"]["invalidParams"][0]["name"],
        "/data/locatie"
    );
    // Sending the same batch again is safe: the first two are replays.
    let (s, b, _) = call(
        &app,
        "POST",
        "/events",
        Some(systeem),
        Some(json!([melding("b1", "MOR-1")])),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(b["results"][0]["replayed"], true);

    let (s, o, _) = call(
        &app,
        "GET",
        "/profiles/nl.vng.meldingen/openapi.json",
        None,
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(o["openapi"], "3.1.0");
    let ev = &o["components"]["schemas"]["Action_nl_vng_meldingen_melding_gedaan"];
    assert_eq!(
        ev["allOf"][1]["properties"]["type"]["const"],
        "nl.vng.meldingen.melding.gedaan"
    );
    assert!(o["paths"]["/events"]["post"]["requestBody"]["content"]
        ["application/cloudevents-batch+json"]
        .is_object());
}

/// A zaak overview: what refers to an aanvraag (a taak, a gesprek and its bijdrage),
/// and one audit trail over all of it, each with the reader's own rules.
#[tokio::test]
async fn related_and_audit_follow_references() {
    let app = app();
    install(&app).await;
    let olo = "dev:https://id.example.nl/omgevingsloket|rol=systeem";
    let jan = "dev:https://id.example.nl/jan|rol=inwoner";
    let jan_id = "https://id.example.nl/jan";
    let femke = "dev:https://id.example.nl/femke|rol=medewerker|team=vergunningen";
    let femke_id = "https://id.example.nl/femke";
    let zaak = "https://zaken.demodam.nl/vergunningen/OV-1";
    let g = "urn:uuid:gesprek-ov-1";
    for (token, e) in [
        (
            olo,
            ev(
                "v1",
                "nl.vng.vergunningen.aanvraag.ingediend",
                zaak,
                json!({ "resourceData": {
            "zaaknummer": "OV-1", "activiteiten": ["bouwen"], "omschrijving": "Dakkapel",
            "aanvrager": { "partij": jan_id, "rol": "aanvrager" } } }),
            ),
        ),
        (
            femke,
            ev(
                "v2",
                "nl.vng.taken.taak.aangemaakt",
                "urn:uuid:taak-1",
                json!({ "resourceData": {
            "titel": "Lever de constructietekening aan", "zaak": zaak, "voor": { "partij": jan_id }, "team": "vergunningen" } }),
            ),
        ),
        (
            femke,
            ev(
                "v3",
                "nl.vng.gesprekken.gesprek.gestart",
                g,
                json!({ "type": "nl.vng.gesprekken.gesprek", "resourceData": {
            "gespreksonderwerp": "Aanvulling nodig voor OV-1", "team": "vergunningen", "aanleidingZaak": zaak,
            "gespreksdeelnames": [ { "type": "actor", "actor": femke_id, "naam": "Femke" }, { "type": "partij", "partij": jan_id, "naam": "Jan" } ] } }),
            ),
        ),
        (
            jan,
            ev(
                "v4",
                "nl.vng.gesprekken.bijdrage.toegevoegd",
                "urn:uuid:b-ov-1",
                json!({ "resourceData": {
            "gesprek": g, "tekstGespreksBijdrage": "Komt eraan." } }),
            ),
        ),
        (
            jan,
            ev(
                "v5",
                "nl.vng.taken.taak.afgerond",
                "urn:uuid:taak-1",
                json!({ "patch": {} }),
            ),
        ),
    ] {
        let (s, b, _) = call(&app, "POST", "/events", Some(token), Some(e)).await;
        assert_eq!(s, StatusCode::CREATED, "{b}");
    }
    let uri = |what: &str| format!("/subjects/{}/{what}", enc(zaak));

    let (s, b, _) = call(&app, "GET", &uri("related"), Some(jan), None).await;
    assert_eq!(s, StatusCode::OK, "{b}");
    let mut types: Vec<&str> = b["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["_meta"]["type"].as_str().unwrap())
        .collect();
    types.sort();
    assert_eq!(types, ["nl.vng.gesprekken.gesprek", "nl.vng.taken.taak"]);

    // The audit trail goes two levels deep: the bijdrage in the gesprek is in it.
    let (_, b, _) = call(&app, "GET", &uri("audit"), Some(jan), None).await;
    let ids: Vec<&str> = b["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["v1", "v2", "v3", "v4", "v5"]);
    assert_eq!(b["events"][4]["actor"], jan_id);

    // The KCC sees the zaak limited and not the bijdrage at all.
    let (_, b, _) = call(&app, "GET", &uri("audit"), Some(KIM), None).await;
    let ids: Vec<&str> = b["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["id"].as_str().unwrap())
        .collect();
    assert!(!ids.contains(&"v4"), "{ids:?}");
    assert!(b["events"]
        .as_array()
        .unwrap()
        .iter()
        .all(|e| e.get("data").is_none()));
    // A stranger gets nothing, not even that it exists.
    let (s, _, _) = call(&app, "GET", &uri("related"), Some(KAREL), None).await;
    assert_eq!(s, StatusCode::NOT_FOUND);

    // The profile list says which types stand on their own (for an overview).
    let (_, b, _) = call(&app, "GET", "/profiles", None, None).await;
    let standalone: Vec<String> = b["profiles"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|p| {
            p["standalone"]
                .as_array()
                .unwrap()
                .iter()
                .map(|t| t.as_str().unwrap().to_string())
        })
        .collect();
    assert!(standalone.contains(&"nl.vng.taken.taak".to_string()));
    assert!(!standalone.contains(&"nl.vng.gesprekken.bijdrage".to_string()));
}

#[tokio::test]
async fn mijnzaken_and_mijntaken_for_the_burger_from_the_token() {
    let app = app();
    install(&app).await;
    let olo = "dev:https://id.example.nl/omgevingsloket|rol=systeem";
    let jan = "dev:https://id.example.nl/jan|rol=inwoner";
    let jan_id = "https://id.example.nl/jan";
    let priya = "dev:https://id.example.nl/priya|rol=inwoner";
    let femke = "dev:https://id.example.nl/femke|rol=medewerker|team=vergunningen";
    let zaak = "https://zaken.demodam.nl/vergunningen/OV-7";
    for (token, e) in [
        (
            olo,
            ev(
                "m1",
                "nl.vng.vergunningen.aanvraag.ingediend",
                zaak,
                json!({ "resourceData": {
            "zaaknummer": "OV-7", "activiteiten": ["bouwen"], "omschrijving": "Dakkapel",
            "bijlagen": ["tekening.pdf"], "aanvrager": { "partij": jan_id, "rol": "aanvrager" } } }),
            ),
        ),
        (
            femke,
            ev(
                "m2",
                "nl.vng.vergunningen.aanvraag.ontvankelijk_verklaard",
                zaak,
                json!({ "patch": { "uiterlijkeBeslisdatum": "2026-12-31" } }),
            ),
        ),
        (
            femke,
            ev(
                "m3",
                "nl.vng.taken.taak.aangemaakt",
                "urn:uuid:0193a0c4-0000-7000-8000-000000000001",
                json!({ "resourceData": {
            "titel": "Lever de constructietekening aan", "zaak": zaak, "voor": { "partij": jan_id },
            "team": "vergunningen", "deadline": "2026-12-08" } }),
            ),
        ),
    ] {
        let (s, b, _) = call(&app, "POST", "/events", Some(token), Some(e)).await;
        assert_eq!(s, StatusCode::CREATED, "{b}");
    }

    let (s, _, _) = call(&app, "GET", "/mijn/zaken/zaken", None, None).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);

    let (s, list, h) = call(&app, "GET", "/mijn/zaken/zaken", Some(jan), None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(h["api-version"], "next");
    assert_eq!(list["count"], 1);
    let z = &list["results"][0];
    assert_eq!(z["zaaknummer"], "OV-7");
    assert_eq!(z["urn"], "urn:nl:gemeenten:zaak:OV-7");
    assert_eq!(z["status"], "open");
    assert_eq!(z["huidigeStatus"], "In behandeling");
    let uuid = z["uuid"].as_str().unwrap();

    let (s, d, _) = call(
        &app,
        "GET",
        &format!("/mijn/zaken/zaken/{uuid}"),
        Some(jan),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let titels: Vec<_> = d["statushistorie"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| (s["titel"].clone(), s["status"].clone()))
        .collect();
    assert_eq!(
        titels,
        vec![
            (json!("Ingediend"), json!("voltooid")),
            (json!("In behandeling"), json!("lopend")),
            (json!("Besloten"), json!("gepland")),
        ]
    );
    assert_eq!(d["documenten"][0]["naam"], "tekening.pdf");

    // Not betrokken: an empty list and the same 404 as a missing zaak.
    let (_, list, _) = call(&app, "GET", "/mijn/zaken/zaken", Some(priya), None).await;
    assert_eq!(list["count"], 0);
    let (s, _, _) = call(
        &app,
        "GET",
        &format!("/mijn/zaken/zaken/{uuid}"),
        Some(priya),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    // A medewerker may read the aanvraag, but it is not *her* zaak.
    let (_, list, _) = call(&app, "GET", "/mijn/zaken/zaken", Some(femke), None).await;
    assert_eq!(list["count"], 0);

    let ctx = "urn%3Anl%3Agemeenten%3Azaak%3AOV-7";
    let (s, taken, _) = call(
        &app,
        "GET",
        &format!("/mijn/taken/taken?status=open&context={ctx}"),
        Some(jan),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(taken["count"], 1);
    let t = &taken["results"][0];
    assert_eq!(t["uuid"], "0193a0c4-0000-7000-8000-000000000001");
    assert_eq!(t["titel"]["nl"], "Lever de constructietekening aan");
    assert_eq!(t["deadline"], "2026-12-08T23:59:59+01:00");
    assert_eq!(t["context"]["urn"], "urn:nl:gemeenten:zaak:OV-7");
    let (s, _, _) = call(
        &app,
        "GET",
        "/mijn/taken/taken/0193a0c4-0000-7000-8000-000000000001",
        Some(jan),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let (_, taken, _) = call(&app, "GET", "/mijn/taken/taken", Some(priya), None).await;
    assert_eq!(taken["count"], 0);
}

#[tokio::test]
async fn mine_is_a_rule_in_the_profile() {
    let app = app();
    install(&app).await;
    start_gesprek(&app).await;
    call(
        &app,
        "POST",
        "/events",
        Some(SUZANNE),
        Some(bijdrage("e2", B1, json!("Hallo"))),
    )
    .await;

    // Over all types with a mine rule, without a type.
    let (s, b, _) = call(
        &app,
        "POST",
        "/query",
        Some(SUZANNE),
        Some(json!({ "mine": true })),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{b}");
    let mut types: Vec<&str> = b["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["_meta"]["type"].as_str().unwrap())
        .collect();
    types.sort();
    assert_eq!(
        types,
        ["nl.vng.gesprekken.bijdrage", "nl.vng.gesprekken.gesprek"]
    );
    assert_eq!(b["feedCursor"], "2");
    // Paging with the cursor gives the same, one at a time.
    let (_, p1, _) = call(
        &app,
        "POST",
        "/query",
        Some(SUZANNE),
        Some(json!({ "mine": true, "pageSize": 1 })),
    )
    .await;
    assert_eq!(p1["results"].as_array().unwrap().len(), 1);
    let (_, p2, _) = call(
        &app,
        "POST",
        "/query",
        Some(SUZANNE),
        Some(json!({ "mine": true, "pageSize": 1, "cursor": p1["next"] })),
    )
    .await;
    assert_eq!(p2["results"].as_array().unwrap().len(), 1);
    assert_ne!(
        p1["results"][0]["_meta"]["subject"],
        p2["results"][0]["_meta"]["subject"]
    );

    // Merve may read the gesprek (her team), but it is not hers.
    let (_, b, _) = call(
        &app,
        "POST",
        "/query",
        Some(MERVE),
        Some(json!({ "mine": true })),
    )
    .await;
    assert_eq!(b["results"].as_array().unwrap().len(), 0);
    let (_, b, _) = call(
        &app,
        "POST",
        "/query",
        Some(MERVE),
        Some(json!({ "type": "nl.vng.gesprekken.gesprek" })),
    )
    .await;
    assert_eq!(b["results"].as_array().unwrap().len(), 1);

    // On the event listing, mine marks what is about the caller.
    let (_, b, _) = call(&app, "GET", "/events?format=json", Some(SUZANNE), None).await;
    assert_eq!(b["events"][0]["mine"], true);
    let (_, b, _) = call(&app, "GET", "/events?format=json", Some(MERVE), None).await;
    assert!(b["events"][0].get("mine").is_none());

    // Without a type and without mine is an error.
    let (s, _, _) = call(&app, "POST", "/query", Some(SUZANNE), Some(json!({}))).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);

    let (_, me, _) = call(&app, "GET", "/me", Some(SUZANNE), None).await;
    assert_eq!(me["id"], SUZANNE_ID);
}

#[tokio::test]
async fn indexes_follow_changes() {
    let app = app();
    install(&app).await;
    let femke = "dev:https://id.example.nl/femke|rol=medewerker|team=vergunningen";
    let jan = "dev:https://id.example.nl/jan|rol=inwoner";
    let (s, b, _) = call(&app, "POST", "/events", Some(femke), Some(ev("t1", "nl.vng.taken.taak.aangemaakt", "urn:taak:1",
        json!({ "resourceData": { "titel": "Stuur een foto", "voor": { "partij": "https://id.example.nl/jan" }, "team": "vergunningen" } })))).await;
    assert_eq!(s, StatusCode::CREATED, "{b}");
    let count = |b: &Value| b["results"].as_array().unwrap().len();
    let q = |f: &str| json!({ "type": "nl.vng.taken.taak", "filter": f });
    let (_, b, _) = call(
        &app,
        "POST",
        "/query",
        Some(femke),
        Some(q("status = 'open'")),
    )
    .await;
    assert_eq!(count(&b), 1, "{b}");
    let (_, b, _) = call(
        &app,
        "POST",
        "/query",
        Some(jan),
        Some(json!({ "mine": true })),
    )
    .await;
    assert_eq!(count(&b), 1, "{b}");
    call(
        &app,
        "POST",
        "/events",
        Some(jan),
        Some(ev(
            "t2",
            "nl.vng.taken.taak.afgerond",
            "urn:taak:1",
            json!({ "patch": {} }),
        )),
    )
    .await;
    let (_, b, _) = call(
        &app,
        "POST",
        "/query",
        Some(femke),
        Some(q("status = 'open'")),
    )
    .await;
    assert_eq!(count(&b), 0, "the old value left the index: {b}");
    let (_, b, _) = call(
        &app,
        "POST",
        "/query",
        Some(femke),
        Some(q("status IN ('afgerond', 'vervallen')")),
    )
    .await;
    assert_eq!(count(&b), 1, "{b}");
}

#[tokio::test]
async fn purge_removes_a_resource_and_what_refers_to_it() {
    let app = app();
    install(&app).await;
    start_gesprek(&app).await;
    let (s, b, _) = call(
        &app,
        "POST",
        "/events",
        Some(SUZANNE),
        Some(bijdrage(
            "b1",
            "urn:uuid:bijdrage-1",
            json!("Mijn adres is Dorpsstraat 1"),
        )),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{b}");
    let uri = format!("/subjects/{}?cascade=true", enc(G1));
    let (s, _, _) = call(&app, "DELETE", &uri, Some(SUZANNE), None).await;
    assert_eq!(s, StatusCode::FORBIDDEN, "only an admin may purge");
    let (s, b, _) = call(&app, "DELETE", &uri, Some(ADMIN), None).await;
    assert_eq!(s, StatusCode::OK, "{b}");
    assert_eq!(b["purged"].as_array().unwrap().len(), 2, "{b}");
    for subject in [G1, "urn:uuid:bijdrage-1"] {
        let (s, _, _) = call(
            &app,
            "GET",
            &format!("/subjects/{}", enc(subject)),
            Some(ADMIN),
            None,
        )
        .await;
        assert_eq!(s, StatusCode::NOT_FOUND);
    }
    let (_, b, _) = call(&app, "GET", "/events", Some(ADMIN), None).await;
    let text = b.to_string();
    assert!(
        !text.contains("Dorpsstraat"),
        "no data left in the log: {text}"
    );
    assert!(!text.contains("Vraag over mijn aanvraag"), "{text}");
    assert!(text.contains("redacted"), "the log keeps a shell: {text}");
    let (_, b, _) = call(&app, "GET", "/events", Some(SUZANNE), None).await;
    assert_eq!(b["events"].as_array().map(|a| a.len()), Some(0), "{b}");
    let (_, b, _) = call(
        &app,
        "POST",
        "/query",
        Some(SUZANNE),
        Some(json!({ "mine": true })),
    )
    .await;
    assert_eq!(b["results"].as_array().unwrap().len(), 0, "{b}");
    // The same CloudEvent id may be used again: idempotency records are gone too.
    start_gesprek(&app).await;
}

#[tokio::test]
async fn retention_purges_closed_gesprekken_after_a_year() {
    let kv: Arc<dyn KvStore> = Arc::new(MemoryStore::new());
    let app = app_with(kv.clone());
    install(&app).await;
    start_gesprek(&app).await;
    let reg = Register::open(kv.clone()).unwrap();
    let later = chrono::Utc::now() + chrono::Duration::days(800);
    assert!(
        reg.sweep(later).await.unwrap().is_empty(),
        "an open gesprek stays"
    );
    let (s, b, _) = call(
        &app,
        "POST",
        "/events",
        Some(SUZANNE),
        Some(ev(
            "e9",
            "nl.vng.gesprekken.gesprek.afgesloten",
            G1,
            json!({ "patch": { "afsluitreden": "beantwoord" } }),
        )),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{b}");
    let reg = Register::open(kv).unwrap();
    assert!(
        reg.sweep(chrono::Utc::now()).await.unwrap().is_empty(),
        "not yet"
    );
    assert_eq!(reg.sweep(later).await.unwrap(), vec![G1.to_string()]);
}

#[tokio::test]
async fn health_and_metrics() {
    let app = app();
    install(&app).await;
    start_gesprek(&app).await;
    let (s, b, _) = call(&app, "GET", "/health", None, None).await;
    assert_eq!((s, b), (StatusCode::OK, json!("ok")));
    let (s, b, _) = call(&app, "GET", "/metrics", None, None).await;
    assert_eq!(s, StatusCode::OK);
    let text = b.as_str().unwrap();
    assert!(text.contains("openevents_events_total 1"), "{text}");
    assert!(text.contains("openevents_notificaties_total{outcome=\"stored\"}"), "{text}");
    assert!(!text.contains("openevents_last_event_timestamp_seconds 0\n"), "{text}");
}

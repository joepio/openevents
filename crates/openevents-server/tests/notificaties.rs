//! ZGW Notificaties become events: a fake Open Zaak serves the resources, notificaties
//! arrive out of order, twice, and for resources no profile wants.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use axum::body::Body;
use axum::extract::Path;
use axum::http::{HeaderMap, Request, StatusCode};
use axum::{routing::get, Json, Router};
use http_body_util::BodyExt;
use openevents_core::kv::MemoryStore;
use openevents_core::Register;
use openevents_server::auth::{AuthConfig, Tickets};
use openevents_server::notificaties::NotificatiesConfig;
use openevents_server::{router, AppState};
use serde_json::{json, Value};
use tower::ServiceExt;

const AUTH: &str = "Bearer geheim-van-open-notificaties";
const BEZWAAR: &str = "https://openzaak.demodam.nl/catalogi/api/v1/zaaktypen/bezwaar";

async fn fake_open_zaak(
    fetches: Arc<AtomicUsize>,
    afgesloten: Arc<std::sync::atomic::AtomicBool>,
) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let b = base.clone();
    let app = Router::new().route(
        "/zaken/api/v1/{resource}/{id}",
        get(move |Path((resource, id)): Path<(String, String)>, headers: HeaderMap| {
            let b = b.clone();
            let fetches = fetches.clone();
            let afgesloten = afgesloten.clone();
            async move {
                // ZGW auth: a JWT signed with the shared secret.
                let auth = headers.get("authorization").and_then(|h| h.to_str().ok()).unwrap_or("");
                if !auth.starts_with("Bearer ey") {
                    return (StatusCode::FORBIDDEN, Json(json!({}))).into_response();
                }
                fetches.fetch_add(1, Ordering::SeqCst);
                let url = format!("{b}/zaken/api/v1/{resource}/{id}");
                let zaak = format!("{b}/zaken/api/v1/zaken/z1");
                let body = match resource.as_str() {
                    "zaken" => json!({
                        "url": url, "identificatie": "ZAAK-2026-0001", "bronorganisatie": "123456782",
                        "omschrijving": "Bezwaar tegen WOZ-beschikking", "zaaktype": BEZWAAR,
                        "startdatum": "2026-11-10", "vertrouwelijkheidaanduiding": "vertrouwelijk",
                        "status": format!("{b}/zaken/api/v1/statussen/s1"),
                        "einddatum": if afgesloten.load(Ordering::SeqCst) { json!("2026-11-12") } else { Value::Null }
                    }),
                    "statussen" => json!({
                        "url": url, "zaak": zaak, "statustype": format!("{b}/catalogi/api/v1/statustypen/in-behandeling"),
                        "datumStatusGezet": "2026-11-10T10:00:00Z", "statustoelichting": "In behandeling genomen"
                    }),
                    "rollen" => json!({
                        "url": url, "zaak": zaak, "omschrijvingGeneriek": "initiator", "betrokkeneType": "natuurlijk_persoon",
                        "roltype": format!("{b}/catalogi/api/v1/roltypen/initiator"), "betrokkene": "",
                        "betrokkeneIdentificatie": { "inpBsn": "999993653" }
                    }),
                    _ => return (StatusCode::NOT_FOUND, Json(json!({}))).into_response(),
                };
                (StatusCode::OK, Json(body)).into_response()
            }
        }),
    );
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    base
}

use axum::response::IntoResponse;

fn app(register: Arc<Register>) -> Router {
    router(AppState {
        register,
        auth: AuthConfig {
            dev_auth: true,
            admins: vec!["urn:admin".into()],
            ..Default::default()
        },
        tickets: Arc::new(Tickets::default()),
        notificaties: Some(Arc::new(NotificatiesConfig {
            auth: AUTH.into(),
            zgw_client_id: Some("openevents".into()),
            zgw_secret: Some("zgw-secret".into()),
            actor: "urn:openevents:notificaties".into(),
            client: reqwest::Client::new(),
            subscribe: None,
        })),
    })
}

async fn call(
    app: &Router,
    method: &str,
    path: &str,
    auth: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut req = Request::builder()
        .method(method)
        .uri(path)
        .header("authorization", auth);
    let req = match body {
        Some(b) => {
            req = req.header("content-type", "application/json");
            req.body(Body::from(b.to_string())).unwrap()
        }
        None => req.body(Body::empty()).unwrap(),
    };
    let res = app.clone().oneshot(req).await.unwrap();
    let s = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (s, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

fn notificatie(base: &str, resource: &str, path: &str, actie: &str) -> Value {
    json!({
        "kanaal": "zaken",
        "hoofdObject": format!("{base}/zaken/api/v1/zaken/z1"),
        "resource": resource,
        "resourceUrl": format!("{base}/zaken/api/v1/{path}"),
        "actie": actie,
        "aanmaakdatum": "2026-11-10T10:00:01Z",
        "kenmerken": { "bronorganisatie": "123456782", "zaaktype": BEZWAAR, "vertrouwelijkheidaanduiding": "vertrouwelijk" }
    })
}

#[tokio::test]
async fn notificaties_become_a_zaakspiegel() {
    let fetches = Arc::new(AtomicUsize::new(0));
    let afgesloten = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let base = fake_open_zaak(fetches.clone(), afgesloten.clone()).await;
    let register = Arc::new(Register::open(Arc::new(MemoryStore::new())).unwrap());
    let app = app(register);
    let profile: Value =
        serde_json::from_str(include_str!("../../../examples/profiles/zgw-zaken.json")).unwrap();
    let (s, b) = call(
        &app,
        "PUT",
        "/profiles/nl.vng.zgw.zaken",
        "Bearer dev:urn:admin",
        Some(profile),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{b}");

    // Without the right Authorization header: refused.
    let n = notificatie(&base, "status", "statussen/s1", "create");
    let (s, _) = call(
        &app,
        "POST",
        "/notificaties",
        "Bearer iets-anders",
        Some(n.clone()),
    )
    .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);

    // The status arrives before the zaak: the zaak is mirrored first.
    let (s, b) = call(&app, "POST", "/notificaties", AUTH, Some(n.clone())).await;
    assert_eq!(s, StatusCode::NO_CONTENT, "{b}");
    // Delivered twice: stored once.
    let (s, _) = call(&app, "POST", "/notificaties", AUTH, Some(n)).await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    // The zaak.create comes in late: already mirrored, fine.
    let (s, _) = call(
        &app,
        "POST",
        "/notificaties",
        AUTH,
        Some(notificatie(&base, "zaak", "zaken/z1", "create")),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (s, _) = call(
        &app,
        "POST",
        "/notificaties",
        AUTH,
        Some(notificatie(&base, "rol", "rollen/r1", "create")),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    // No profile wants zaakeigenschappen: acknowledged, not fetched, not stored.
    let before = fetches.load(Ordering::SeqCst);
    let (s, _) = call(
        &app,
        "POST",
        "/notificaties",
        AUTH,
        Some(notificatie(
            &base,
            "zaakeigenschap",
            "zaakeigenschappen/e1",
            "create",
        )),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    assert_eq!(fetches.load(Ordering::SeqCst), before);

    let zaak = format!("{base}/zaken/api/v1/zaken/z1")
        .replace(':', "%3A")
        .replace('/', "%2F");
    let (s, z) = call(
        &app,
        "GET",
        &format!("/subjects/{zaak}"),
        &format!("Bearer dev:https://id.example.nl/behandelaar|rol=medewerker|zaaktypen={BEZWAAR}"),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{z}");
    assert_eq!(z["identificatie"], "ZAAK-2026-0001");
    // The rol.create also patched the zaak with the initiator's BSN.
    assert_eq!(z["initiatorBsn"][0], "999993653");
    // Subresources are resources of their own, readable through their zaak.
    let behandelaar =
        format!("Bearer dev:https://id.example.nl/behandelaar|rol=medewerker|zaaktypen={BEZWAAR}");
    let status = z["status"]
        .as_str()
        .unwrap()
        .replace(':', "%3A")
        .replace('/', "%2F");
    let (s, st) = call(
        &app,
        "GET",
        &format!("/subjects/{status}"),
        &behandelaar,
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{st}");
    assert_eq!(st["statustoelichting"], "In behandeling genomen");
    let (s, q) = call(
        &app,
        "POST",
        "/query",
        &behandelaar,
        Some(json!({
        "type": "nl.vng.zgw.status", "filter": format!("zaak.identificatie = 'ZAAK-2026-0001'") })),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{q}");
    assert_eq!(q["results"].as_array().unwrap().len(), 1);

    // An update carries only the change; an update without change stores nothing.
    let mut upd = notificatie(&base, "zaak", "zaken/z1", "partial_update");
    upd["aanmaakdatum"] = json!("2026-11-10T11:00:00Z");
    let (s, _) = call(&app, "POST", "/notificaties", AUTH, Some(upd.clone())).await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    afgesloten.store(true, Ordering::SeqCst);
    upd["aanmaakdatum"] = json!("2026-11-12T09:00:00Z");
    let (s, _) = call(&app, "POST", "/notificaties", AUTH, Some(upd)).await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (_, tl) = call(
        &app,
        "GET",
        &format!("/subjects/{zaak}/events"),
        "Bearer dev:urn:admin",
        None,
    )
    .await;
    let types: Vec<&str> = tl["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["type"].as_str().unwrap())
        .collect();
    assert_eq!(
        types,
        [
            "nl.vng.zgw.zaken.zaak.create",
            "nl.vng.zgw.zaken.rol.create",
            "nl.vng.zgw.zaken.zaak.partial_update"
        ],
        "{tl}"
    );
    assert_eq!(
        tl["events"][2]["data"]["patch"],
        json!({ "einddatum": "2026-11-12" })
    );
    let (_, z) = call(
        &app,
        "GET",
        &format!("/subjects/{zaak}"),
        "Bearer dev:urn:admin",
        None,
    )
    .await;
    assert_eq!(z["einddatum"], "2026-11-12");
    assert_eq!(z["initiatorBsn"][0], "999993653");

    // The initiator sees her zaak through the BSN in her token; someone else does not.
    let (s, _) = call(
        &app,
        "GET",
        &format!("/subjects/{zaak}"),
        "Bearer dev:https://id.example.nl/priya|rol=inwoner|bsn=999993653",
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let (s, _) = call(
        &app,
        "GET",
        &format!("/subjects/{zaak}"),
        "Bearer dev:https://id.example.nl/karel|rol=inwoner|bsn=999990019",
        None,
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    // A medewerker for another zaaktype does not see it either.
    let (s, _) = call(
        &app,
        "GET",
        &format!("/subjects/{zaak}"),
        "Bearer dev:https://id.example.nl/x|rol=medewerker|zaaktypen=https://andere",
        None,
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

/// OpenEvents subscribes itself, once, and again after Open Notificaties was reset.
#[tokio::test]
async fn subscribes_itself_at_open_notificaties() {
    use std::sync::Mutex;
    let store: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(vec![]));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let (s1, s2) = (store.clone(), store.clone());
    let nrc = Router::new().route(
        "/api/v1/abonnement",
        get(move || {
            let s = s1.clone();
            async move { Json(Value::Array(s.lock().unwrap().clone())) }
        })
        .post(move |Json(b): Json<Value>| {
            let s = s2.clone();
            async move {
                s.lock().unwrap().push(b);
                StatusCode::CREATED
            }
        }),
    );
    tokio::spawn(async move { axum::serve(listener, nrc).await.unwrap() });
    let cfg = NotificatiesConfig {
        auth: AUTH.into(),
        zgw_client_id: Some("openevents".into()),
        zgw_secret: Some("zgw-secret".into()),
        actor: "urn:openevents:notificaties".into(),
        client: reqwest::Client::new(),
        subscribe: Some(openevents_server::notificaties::Subscribe {
            api: format!("{base}/api/v1"),
            kanalen: vec!["zaken".into()],
            callback: "https://events.demostack.nl/notificaties".into(),
        }),
    };
    assert_eq!(cfg.ensure_subscribed().await, Ok(true));
    assert_eq!(cfg.ensure_subscribed().await, Ok(false));
    {
        let s = store.lock().unwrap();
        assert_eq!(s.len(), 1);
        assert_eq!(s[0]["auth"], AUTH);
        assert_eq!(s[0]["kanalen"][0]["naam"], "zaken");
    }
    store.lock().unwrap().clear(); // Open Notificaties was reset
    assert_eq!(cfg.ensure_subscribed().await, Ok(true));
}

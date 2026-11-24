//! HTTP API of OpenEvents (docs/architectuur.md, "API-oppervlak").

pub mod auth;
pub mod backup;
pub mod metrics;
pub mod mijn;
pub mod notificaties;
pub mod openapi;
pub mod pseudonym;
pub mod schemas;

use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures_util::Stream;
use openevents_core::{Actor, EventFilter, QueryRequest, Register, RegisterError};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::broadcast::error::RecvError;

use crate::auth::{AuthConfig, Tickets};
use crate::notificaties::NotificatiesConfig;

#[derive(Clone)]
pub struct AppState {
    pub register: Arc<Register>,
    pub auth: AuthConfig,
    pub tickets: Arc<Tickets>,
    /// `POST /notificaties`, only when configured.
    pub notificaties: Option<Arc<NotificatiesConfig>>,
    /// Shared JSON Schemas that profiles refer to (`$ref` to the VNG API Lab).
    pub schemas: Arc<schemas::Schemas>,
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/health", get(health))
        .route("/metrics", get(metrics))
        .route("/me", get(get_me))
        .route("/events", get(get_events).post(post_events))
        .route("/events/tickets", post(post_ticket))
        .route("/notificaties", post(post_notificatie))
        .route(
            "/subjects/{subject}",
            get(get_subject).delete(delete_subject),
        )
        .route("/subjects/{subject}/events", get(get_timeline))
        .route("/subjects/{subject}/related", get(get_related))
        .route("/subjects/{subject}/audit", get(get_audit))
        .route("/query", post(post_query))
        .route("/profiles", get(list_profiles))
        .route("/profiles/{id}", get(get_profile).put(put_profile))
        .route("/profiles/{id}/openapi.json", get(get_profile_openapi))
        .route("/mijn/zaken/zaken", get(mijn::list_zaken))
        .route("/mijn/zaken/zaken/{uuid}", get(mijn::get_zaak))
        .route("/mijn/taken/taken", get(mijn::list_taken))
        .route("/mijn/taken/taken/{uuid}", get(mijn::get_taak))
        .route("/mijnoverheid", get(mijnoverheid))
        .route("/mijnoverheid/", get(mijnoverheid))
        .route("/ui", get(ui))
        .route("/ui/", get(ui))
        .route("/ui/config", get(ui_config))
        .with_state(state)
}

// ---------- errors ----------

fn problem(status: StatusCode, title: &str, detail: &str, params: Option<Value>) -> Response {
    let mut body = json!({ "type": "about:blank", "title": title, "status": status.as_u16(), "detail": detail });
    if let Some(p) = params {
        body["invalidParams"] = p;
    }
    let mut res = (status, body.to_string()).into_response();
    res.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/problem+json"),
    );
    res
}

fn error_response(e: RegisterError) -> Response {
    match e {
        RegisterError::Invalid(detail, params) => problem(
            StatusCode::UNPROCESSABLE_ENTITY,
            "Rejected, nothing stored",
            &detail,
            Some(json!(params)),
        ),
        // One answer without details, so callers cannot probe what exists (A3).
        RegisterError::Forbidden => {
            problem(StatusCode::FORBIDDEN, "Forbidden", "Not allowed.", None)
        }
        RegisterError::NotFound => problem(StatusCode::NOT_FOUND, "Not found", "Not found.", None),
        RegisterError::Stale(params) => problem(
            StatusCode::PRECONDITION_FAILED,
            "Changed in the meantime",
            "previousCommit does not match the last commit.",
            Some(json!(params)),
        ),
        RegisterError::Conflict(detail) => problem(StatusCode::CONFLICT, "Conflict", &detail, None),
        RegisterError::Storage(e) => {
            tracing::error!("{e}");
            problem(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Storage error",
                "Try again later.",
                None,
            )
        }
    }
}

fn unauthorized() -> Response {
    let mut res = problem(
        StatusCode::UNAUTHORIZED,
        "Unauthorized",
        "A valid bearer token is required.",
        None,
    );
    res.headers_mut()
        .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
    res
}

#[allow(clippy::result_large_err)]
/// The actor of a request that writes: dev tokens are refused when they are read-only.
async fn writer(state: &AppState, headers: &HeaderMap) -> Result<Actor, Response> {
    if !state.auth.may_write(headers) {
        return Err(problem(
            StatusCode::FORBIDDEN,
            "Read only",
            "Persona's zijn hier alleen om te bekijken. Log in om iets te doen.",
            None,
        ));
    }
    actor(state, headers).await
}

async fn actor(state: &AppState, headers: &HeaderMap) -> Result<Actor, Response> {
    state
        .auth
        .from_headers(headers)
        .await
        .ok_or_else(unauthorized)
}

// ---------- handlers ----------

async fn index() -> Json<Value> {
    Json(json!({
        "name": "OpenEvents",
        "version": env!("CARGO_PKG_VERSION"),
        "links": {
            "events": "/events",
            "query": "/query",
            "profiles": "/profiles",
            "docs": "https://github.com/joepio/openevents/tree/main/docs"
        }
    }))
}

async fn post_events(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Option<Json<Value>>,
) -> Response {
    let actor = match writer(&state, &headers).await {
        Ok(a) => a,
        Err(r) => return r,
    };
    let Some(Json(body)) = body else {
        return problem(
            StatusCode::BAD_REQUEST,
            "Bad request",
            "Body must be JSON (a CloudEvent).",
            None,
        );
    };
    if let Value::Array(events) = body {
        return post_batch(&state, &actor, events).await;
    }
    match state.register.submit(&actor, body).await {
        Ok(ok) => {
            let mut res = (StatusCode::CREATED, Json(ok.body)).into_response();
            if ok.replayed {
                res.headers_mut()
                    .insert("Idempotent-Replayed", HeaderValue::from_static("true"));
            }
            res
        }
        Err(e) => error_response(e),
    }
}

/// Most events in one batch.
pub const MAX_BATCH: usize = 500;

/// A batch (`application/cloudevents-batch+json`, a JSON array): every event is handled
/// on its own, in order, as if it was posted alone. The answer lists the outcome per
/// event. All or nothing is what `json.commit.transaction` is for.
async fn post_batch(state: &AppState, actor: &Actor, events: Vec<Value>) -> Response {
    if events.len() > MAX_BATCH {
        return problem(
            StatusCode::PAYLOAD_TOO_LARGE,
            "Batch too large",
            &format!("At most {MAX_BATCH} events per batch."),
            None,
        );
    }
    let mut results = Vec::with_capacity(events.len());
    let (mut ok, mut failed) = (0, 0);
    for ev in events {
        let id = ev.get("id").cloned().unwrap_or(Value::Null);
        match state.register.submit(actor, ev).await {
            Ok(r) => {
                ok += 1;
                results.push(
                    json!({ "id": id, "status": 201, "replayed": r.replayed, "body": r.body }),
                );
            }
            Err(e) => {
                failed += 1;
                let res = error_response(e);
                let status = res.status().as_u16();
                let bytes = axum::body::to_bytes(res.into_body(), 64 * 1024)
                    .await
                    .unwrap_or_default();
                let problem: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
                results.push(json!({ "id": id, "status": status, "problem": problem }));
            }
        }
    }
    let status = if failed == 0 {
        StatusCode::OK
    } else {
        StatusCode::MULTI_STATUS
    };
    (
        status,
        Json(json!({ "stored": ok, "failed": failed, "results": results })),
    )
        .into_response()
}

/// Ready when the store answers.
async fn health(State(state): State<AppState>) -> Response {
    match state.register.head() {
        Ok(_) => "ok".into_response(),
        Err(e) => (StatusCode::SERVICE_UNAVAILABLE, e.to_string()).into_response(),
    }
}

/// Prometheus metrics (no personal data: counts and times only).
async fn metrics(State(state): State<AppState>) -> Response {
    let head = state.register.head().unwrap_or(0);
    let last = state
        .register
        .last_event()
        .ok()
        .flatten()
        .and_then(|e| e["time"].as_str().map(String::from))
        .and_then(|t| chrono::DateTime::parse_from_rfc3339(&t).ok())
        .map(|t| t.timestamp().max(0) as u64);
    (
        [(header::CONTENT_TYPE, "text/plain; version=0.0.4")],
        metrics::render(head, last, state.register.stream_clients()),
    )
        .into_response()
}

async fn post_notificatie(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Option<Json<notificaties::Notificatie>>,
) -> Response {
    let Some(cfg) = &state.notificaties else {
        return error_response(RegisterError::NotFound);
    };
    let auth = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok());
    if !cfg.authorized(auth) {
        return unauthorized();
    }
    let Some(Json(n)) = body else {
        return problem(
            StatusCode::BAD_REQUEST,
            "Bad request",
            "Body must be a ZGW notificatie.",
            None,
        );
    };
    let what = format!("{} {} {}", n.kanaal, n.resource, n.actie);
    match notificaties::handle(cfg, &state.register, n).await {
        Ok(notificaties::Outcome::Stored) => {
            tracing::info!("notificatie stored: {what}");
            metrics::notificatie("stored");
            StatusCode::NO_CONTENT.into_response()
        }
        Ok(notificaties::Outcome::Ignored(why)) => {
            tracing::debug!("notificatie ignored: {why}");
            metrics::notificatie("ignored");
            StatusCode::NO_CONTENT.into_response()
        }
        // Open Notificaties retries on an error, which is what we want here.
        Err(e) => {
            tracing::warn!("notificatie failed: {e}");
            metrics::notificatie("failed");
            problem(
                StatusCode::BAD_GATEWAY,
                "Notificatie not processed",
                &e,
                None,
            )
        }
    }
}

async fn post_ticket(State(state): State<AppState>, headers: HeaderMap) -> Response {
    match actor(&state, &headers).await {
        Ok(a) => Json(
            json!({ "ticket": state.tickets.issue(a), "expiresIn": auth::TICKET_TTL.as_secs() }),
        )
        .into_response(),
        Err(r) => r,
    }
}

#[derive(Deserialize, Default)]
struct EventsParams {
    after: Option<String>,
    limit: Option<usize>,
    #[serde(rename = "type")]
    type_prefix: Option<String>,
    subject: Option<String>,
    ticket: Option<String>,
    format: Option<String>,
}

fn parse_cursor(s: Option<&str>) -> u64 {
    s.and_then(|v| v.trim().parse().ok()).unwrap_or(0)
}

async fn get_events(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(p): Query<EventsParams>,
) -> Response {
    let actor = match (&p.ticket, state.auth.from_headers(&headers).await) {
        (_, Some(a)) => a,
        (Some(t), None) => match state.tickets.redeem(t) {
            Some(a) => a,
            None => return unauthorized(),
        },
        (None, None) => return unauthorized(),
    };
    let filter = EventFilter {
        type_prefix: p.type_prefix.clone(),
        subject: p.subject.clone(),
    };
    let wants_sse = p.format.as_deref() != Some("json")
        && headers
            .get(header::ACCEPT)
            .and_then(|v| v.to_str().ok())
            .map(|v| v.contains("text/event-stream"))
            .unwrap_or(false);
    if !wants_sse {
        let after = parse_cursor(p.after.as_deref());
        return match state
            .register
            .list_events(&actor, after, p.limit.unwrap_or(50), &filter)
        {
            Ok((events, cursor)) => {
                let head = state.register.head().unwrap_or(cursor);
                Json(json!({ "events": events, "next": cursor.to_string(), "head": head.to_string(), "hasMore": cursor < head })).into_response()
            }
            Err(e) => error_response(e),
        };
    }
    // SSE: Last-Event-ID is the cursor (ADR-0002). Without one, only new events.
    let last_id = headers
        .get("last-event-id")
        .and_then(|v| v.to_str().ok())
        .map(String::from)
        .or(p.after.clone());
    let start = match last_id {
        Some(id) => parse_cursor(Some(&id)),
        None => state.register.head().unwrap_or(0),
    };
    Sse::new(event_stream(state.register.clone(), actor, filter, start))
        .keep_alive(KeepAlive::default())
        .into_response()
}

fn sse_event(e: &Value) -> Event {
    let mut ev = Event::default().data(e.to_string());
    if let Some(seq) = e["sequence"].as_str() {
        ev = ev.id(seq);
    }
    ev
}

#[allow(clippy::while_let_loop)]
fn event_stream(
    register: Arc<Register>,
    actor: Actor,
    filter: EventFilter,
    start: u64,
) -> impl Stream<Item = Result<Event, Infallible>> {
    async_stream::stream! {
        // Subscribe first, then catch up from the log, so nothing falls in between.
        let mut rx = register.subscribe();
        let mut last = start;
        loop {
            let (events, cursor) = match register.list_events(&actor, last, 100, &filter) {
                Ok(x) => x,
                Err(_) => break,
            };
            for e in &events {
                yield Ok(sse_event(e));
            }
            last = cursor;
            if events.len() < 100 && cursor >= register.head().unwrap_or(cursor) {
                break;
            }
        }
        let mut cache = HashMap::new();
        loop {
            match rx.recv().await {
                Ok(stored) => {
                    if stored.sequence <= last {
                        continue;
                    }
                    last = stored.sequence;
                    // Rights can change with every event, so check against the current state.
                    cache.clear();
                    if register.event_matches(&stored.event, &filter) {
                        if let Ok(Some(e)) = register.present_event(&stored.event, &actor, &mut cache) {
                            yield Ok(sse_event(&e));
                        }
                    }
                }
                Err(RecvError::Lagged(_)) => {
                    // Fell behind: catch up from the log.
                    loop {
                        let Ok((events, cursor)) = register.list_events(&actor, last, 100, &filter) else { break };
                        for e in &events {
                            yield Ok(sse_event(e));
                        }
                        last = cursor;
                        if events.len() < 100 {
                            break;
                        }
                    }
                }
                Err(RecvError::Closed) => break,
            }
        }
    }
}

/// `GET /me`: who the token says you are, as the register sees it (actor id and claims).
async fn get_me(State(state): State<AppState>, headers: HeaderMap) -> Response {
    match actor(&state, &headers).await {
        Ok(a) => Json(json!({ "id": a.id, "admin": a.admin, "claims": a.claims })).into_response(),
        Err(r) => r,
    }
}

async fn get_subject(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(subject): Path<String>,
) -> Response {
    let actor = match actor(&state, &headers).await {
        Ok(a) => a,
        Err(r) => return r,
    };
    match state.register.get_subject(&actor, &subject) {
        Ok(v) => Json(v).into_response(),
        Err(e) => error_response(e),
    }
}

#[derive(Deserialize)]
struct PurgeParams {
    #[serde(default)]
    cascade: bool,
}

/// Remove a resource for good (admin): state, indexes and the data in its events.
async fn delete_subject(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(subject): Path<String>,
    Query(p): Query<PurgeParams>,
) -> Response {
    let actor = match writer(&state, &headers).await {
        Ok(a) => a,
        Err(r) => return r,
    };
    match state.register.purge(&actor, &subject, p.cascade).await {
        Ok(purged) => {
            tracing::info!("purged by {}: {}", actor.id, purged.join(", "));
            metrics::purged(purged.len());
            Json(json!({ "purged": purged })).into_response()
        }
        Err(e) => error_response(e),
    }
}

async fn get_related(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(subject): Path<String>,
) -> Response {
    let actor = match actor(&state, &headers).await {
        Ok(a) => a,
        Err(r) => return r,
    };
    match state.register.related(&actor, &subject) {
        Ok(results) => Json(json!({ "results": results })).into_response(),
        Err(e) => error_response(e),
    }
}

async fn get_audit(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(subject): Path<String>,
) -> Response {
    let actor = match actor(&state, &headers).await {
        Ok(a) => a,
        Err(r) => return r,
    };
    match state.register.audit(&actor, &subject) {
        Ok(v) => Json(v).into_response(),
        Err(e) => error_response(e),
    }
}

async fn get_timeline(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(subject): Path<String>,
    Query(p): Query<EventsParams>,
) -> Response {
    let actor = match actor(&state, &headers).await {
        Ok(a) => a,
        Err(r) => return r,
    };
    match state.register.timeline(
        &actor,
        &subject,
        parse_cursor(p.after.as_deref()),
        p.limit.unwrap_or(50),
    ) {
        Ok(events) => Json(json!({ "events": events })).into_response(),
        Err(e) => error_response(e),
    }
}

async fn post_query(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Option<Json<QueryRequest>>,
) -> Response {
    let actor = match actor(&state, &headers).await {
        Ok(a) => a,
        Err(r) => return r,
    };
    let Some(Json(req)) = body else {
        return problem(
            StatusCode::BAD_REQUEST,
            "Bad request",
            "Body must be a query object with at least type.",
            None,
        );
    };
    match state.register.query(&actor, &req) {
        Ok(v) => Json(v).into_response(),
        Err(e) => error_response(e),
    }
}

async fn list_profiles(State(state): State<AppState>) -> Json<Value> {
    Json(json!({ "profiles": state.register.list_profiles() }))
}

async fn get_profile(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    match state.register.get_profile(&id) {
        Some(p) => Json(p).into_response(),
        None => error_response(RegisterError::NotFound),
    }
}

async fn get_profile_openapi(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    match state.register.get_profile(&id) {
        Some(p) => Json(openapi::for_profile(&p)).into_response(),
        None => error_response(RegisterError::NotFound),
    }
}

async fn put_profile(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    body: Option<Json<Value>>,
) -> Response {
    let actor = match writer(&state, &headers).await {
        Ok(a) => a,
        Err(r) => return r,
    };
    let Some(Json(body)) = body else {
        return problem(
            StatusCode::BAD_REQUEST,
            "Bad request",
            "Body must be a profile (JSON).",
            None,
        );
    };
    // Only an administrator makes the register fetch schemas.
    let body = if actor.admin {
        match state.schemas.resolve_profile(body).await {
            Ok(b) => b,
            Err(e) => {
                return problem(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "Rejected, nothing stored",
                    &format!("Schema could not be loaded: {e}"),
                    None,
                )
            }
        }
    } else {
        body
    };
    match state.register.put_profile(&actor, &id, body) {
        Ok(v) => Json(v).into_response(),
        Err(e) => error_response(e),
    }
}

// ---------- UI ----------

async fn mijnoverheid() -> Response {
    let mut res = include_str!("mijnoverheid.html").into_response();
    res.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    res
}

async fn ui() -> Response {
    let mut res = include_str!("ui.html").into_response();
    res.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    res
}

/// What the UI needs to know. Personas (from the profiles) only in dev mode, because
/// they are logins without a password.
async fn ui_config(State(state): State<AppState>) -> Json<Value> {
    let mut personas = Vec::new();
    if state.auth.dev_auth {
        for admin in &state.auth.admins {
            personas.push(json!({ "id": admin, "label": "Beheerder (ziet alles)", "claims": {}, "admin": true }));
        }
        for p in state.register.list_profiles() {
            if let Some(raw) = p["id"]
                .as_str()
                .and_then(|id| state.register.get_profile(id))
            {
                if let Some(list) = raw["personas"].as_array() {
                    for persona in list {
                        if !personas.iter().any(|x| x["id"] == persona["id"]) {
                            personas.push(persona.clone());
                        }
                    }
                }
            }
        }
    }
    // Login for browser apps such as /mijnoverheid: authorization code with PKCE at the
    // OIDC provider, with a public client. Only the issuer and client id, no secrets.
    let login = match (
        std::env::var("OPENEVENTS_OIDC_ISSUER")
            .ok()
            .filter(|s| !s.is_empty()),
        std::env::var("OPENEVENTS_UI_CLIENT_ID")
            .ok()
            .filter(|s| !s.is_empty()),
    ) {
        (Some(issuer), Some(client_id)) => json!({ "issuer": issuer, "clientId": client_id }),
        _ => Value::Null,
    };
    Json(
        json!({ "devAuth": state.auth.dev_auth, "devReadOnly": state.auth.dev_read_only, "personas": personas, "login": login }),
    )
}

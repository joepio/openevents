//! HTTP API of OpenEvents (docs/architectuur.md, "API-oppervlak").

pub mod auth;

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

#[derive(Clone)]
pub struct AppState {
    pub register: Arc<Register>,
    pub auth: AuthConfig,
    pub tickets: Arc<Tickets>,
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/health", get(|| async { "ok" }))
        .route("/events", get(get_events).post(post_events))
        .route("/events/tickets", post(post_ticket))
        .route("/subjects/{subject}", get(get_subject))
        .route("/subjects/{subject}/events", get(get_timeline))
        .route("/query", post(post_query))
        .route("/profiles", get(list_profiles))
        .route("/profiles/{id}", get(get_profile).put(put_profile))
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
fn actor(state: &AppState, headers: &HeaderMap) -> Result<Actor, Response> {
    state.auth.from_headers(headers).ok_or_else(unauthorized)
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
    let actor = match actor(&state, &headers) {
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

async fn post_ticket(State(state): State<AppState>, headers: HeaderMap) -> Response {
    match actor(&state, &headers) {
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
    let actor = match (&p.ticket, state.auth.from_headers(&headers)) {
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
        let mut cache: HashMap<String, bool> = HashMap::new();
        loop {
            match rx.recv().await {
                Ok(stored) => {
                    if stored.sequence <= last {
                        continue;
                    }
                    last = stored.sequence;
                    // Rights can change with every event, so check against the current state.
                    cache.clear();
                    if register.event_matches(&stored.event, &filter)
                        && register.event_visible(&stored.event, &actor, &mut cache).unwrap_or(false)
                    {
                        yield Ok(sse_event(&stored.event));
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

async fn get_subject(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(subject): Path<String>,
) -> Response {
    let actor = match actor(&state, &headers) {
        Ok(a) => a,
        Err(r) => return r,
    };
    match state.register.get_subject(&actor, &subject) {
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
    let actor = match actor(&state, &headers) {
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
    let actor = match actor(&state, &headers) {
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

async fn put_profile(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    body: Option<Json<Value>>,
) -> Response {
    let actor = match actor(&state, &headers) {
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
    match state.register.put_profile(&actor, &id, body) {
        Ok(v) => Json(v).into_response(),
        Err(e) => error_response(e),
    }
}

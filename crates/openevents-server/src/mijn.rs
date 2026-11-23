//! MijnZaken and MijnTaken (VNG API Lab, version `next`) on top of the register.
//!
//! A portal such as the MijnOverheid demo in the lab talks to these contracts. Here they
//! are a read view over what the register already holds, for the burger from the token:
//!
//! - `GET /mijn/zaken/zaken`, `GET /mijn/zaken/zaken/{uuid}`
//! - `GET /mijn/taken/taken`, `GET /mijn/taken/taken/{uuid}`
//!
//! A zaak is a vergunningaanvraag, a melding or a ZGW zaak (from the Notificaties
//! adapter). The register's read rules apply first; on top of that MijnZaken shows only
//! what the burger is betrokkene in (aanvrager, gemachtigde, melder, initiator), since a
//! read rule may let an inwoner see more (a public melding, a bekendgemaakte aanvraag).
//! See docs/mijnoverheid.md.

use std::collections::HashMap;

use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use openevents_core::{Actor, QueryRequest, Register};
use serde::Deserialize;
use serde_json::{json, Map, Value};

use crate::{actor, AppState};

const AANVRAAG: &str = "nl.vng.vergunningen.aanvraag";
const MELDING: &str = "nl.vng.meldingen.melding";
const ZGW_ZAAK: &str = "nl.vng.zgw.zaak";
const ZGW_STATUS: &str = "nl.vng.zgw.status";
const TAAK: &str = "nl.vng.taken.taak";
const GESPREK: &str = "nl.vng.gesprekken.gesprek";

/// At most this many resources of one type are read for one overview.
const MAX_SCAN_PAGES: usize = 20;

#[derive(Deserialize, Default)]
pub struct ListParams {
    status: Option<String>,
    context: Option<String>,
    page: Option<usize>,
    #[serde(rename = "pageSize")]
    page_size: Option<usize>,
}

// ---------- reading ----------

/// Every resource of a type this actor may read. Unknown types give nothing.
fn all(reg: &Register, actor: &Actor, type_name: &str) -> Vec<Value> {
    let mut out = Vec::new();
    let mut cursor = None;
    for _ in 0..MAX_SCAN_PAGES {
        let req = QueryRequest {
            type_name: type_name.to_string(),
            filter: None,
            fields: None,
            page_size: Some(100),
            cursor: cursor.take(),
            mine: false,
        };
        let Ok(v) = reg.query(actor, &req) else { break };
        if let Some(list) = v["results"].as_array() {
            out.extend(list.iter().cloned());
        }
        match v["next"].as_str() {
            Some(n) => cursor = Some(n.to_string()),
            None => break,
        }
    }
    out
}

fn subject(v: &Value) -> &str {
    v["_meta"]["subject"].as_str().unwrap_or_default()
}

fn s<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str).filter(|s| !s.is_empty())
}

/// The uuid of a resource: from `urn:uuid:…`, otherwise a stable UUIDv5 of its id.
pub fn uuid_of(subject: &str) -> String {
    if let Some(u) = subject.strip_prefix("urn:uuid:") {
        if uuid::Uuid::parse_str(u).is_ok() {
            return u.to_string();
        }
    }
    uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_URL, subject.as_bytes()).to_string()
}

/// The date part of an ISO date-time.
fn date(v: Option<&str>) -> Option<String> {
    v.map(|t| t.chars().take(10).collect())
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

// ---------- zaken ----------

/// Is the burger betrokkene? Read rules decide what may be read; this decides what
/// belongs in *my* zaken.
fn betrokken(type_name: &str, v: &Value, actor: &Actor) -> bool {
    let me = actor.id.as_str();
    match type_name {
        AANVRAAG => v["aanvrager"]["partij"] == me || v["gemachtigde"] == me,
        MELDING => v["melder"]["partij"] == me,
        ZGW_ZAAK => match (&v["initiatorBsn"], &actor.claims["bsn"]) {
            (Value::String(a), Value::String(b)) => a == b,
            _ => false,
        },
        TAAK => v["voor"]["partij"] == me,
        _ => false,
    }
}

struct Zaak {
    type_name: &'static str,
    raw: Value,
    summary: Value,
}

fn zaaknummer(type_name: &str, v: &Value) -> String {
    match type_name {
        ZGW_ZAAK => s(v, "identificatie"),
        _ => s(v, "zaaknummer"),
    }
    .unwrap_or_default()
    .to_string()
}

fn urn_for(nummer: &str, subject: &str) -> String {
    if nummer.is_empty() {
        format!("urn:nl:gemeenten:zaak:{}", uuid_of(subject))
    } else {
        format!("urn:nl:gemeenten:zaak:{nummer}")
    }
}

/// Status and huidigeStatus of a zaak.
fn zaak_status(reg: &Register, actor: &Actor, type_name: &str, v: &Value) -> (bool, String) {
    let st = s(v, "status").unwrap_or_default();
    match type_name {
        AANVRAAG => (
            matches!(st, "besloten" | "ingetrokken"),
            capitalize(if st.is_empty() { "ingediend" } else { st }),
        ),
        MELDING => (
            st == "afgehandeld",
            capitalize(if st.is_empty() { "gemeld" } else { st }),
        ),
        _ => {
            let gesloten = s(v, "einddatum").is_some();
            let laatste = zgw_statussen(reg, actor, subject(v)).pop();
            let titel = laatste
                .and_then(|x| s(&x, "statustoelichting").map(String::from))
                .unwrap_or_else(|| {
                    (if gesloten {
                        "Afgerond"
                    } else {
                        "In behandeling"
                    })
                    .into()
                });
            (gesloten, titel)
        }
    }
}

fn zgw_statussen(reg: &Register, actor: &Actor, zaak: &str) -> Vec<Value> {
    let mut list: Vec<Value> = reg
        .related(actor, zaak)
        .unwrap_or_default()
        .into_iter()
        .filter(|r| r["_meta"]["type"] == ZGW_STATUS)
        .collect();
    list.sort_by(|a, b| {
        s(a, "datumStatusGezet")
            .unwrap_or_default()
            .cmp(s(b, "datumStatusGezet").unwrap_or_default())
    });
    list
}

fn zaak_naam(type_name: &str, v: &Value) -> String {
    match type_name {
        AANVRAAG => s(v, "omschrijving")
            .map(|o| o.trim_end_matches('.').to_string())
            .unwrap_or_else(|| {
                let acts: Vec<&str> = v["activiteiten"]
                    .as_array()
                    .map(|a| a.iter().filter_map(Value::as_str).collect())
                    .unwrap_or_default();
                format!("Aanvraag {}", acts.join(", "))
            }),
        MELDING => format!(
            "Melding {}",
            s(v, "subcategorie")
                .or(s(v, "categorie"))
                .unwrap_or("openbare ruimte")
                .to_lowercase()
        ),
        _ => s(v, "omschrijving").unwrap_or("Zaak").to_string(),
    }
}

fn zaak_datum(type_name: &str, v: &Value) -> String {
    let t = match type_name {
        AANVRAAG => s(v, "ingediend"),
        MELDING => s(v, "gemeld"),
        _ => s(v, "registratiedatum").or(s(v, "startdatum")),
    };
    date(t.or(v["_meta"]["created"].as_str())).unwrap_or_default()
}

fn mijn_zaken(reg: &Register, actor: &Actor) -> Vec<Zaak> {
    let mut out = Vec::new();
    for type_name in [AANVRAAG, MELDING, ZGW_ZAAK] {
        for v in all(reg, actor, type_name) {
            if !betrokken(type_name, &v, actor) {
                continue;
            }
            let nummer = zaaknummer(type_name, &v);
            let (gesloten, huidig) = zaak_status(reg, actor, type_name, &v);
            let summary = json!({
                "uuid": uuid_of(subject(&v)),
                "urn": urn_for(&nummer, subject(&v)),
                "zaaknummer": nummer,
                "naam": zaak_naam(type_name, &v),
                "status": if gesloten { "gesloten" } else { "open" },
                "huidigeStatus": huidig,
                "datumAanvraag": zaak_datum(type_name, &v),
            });
            out.push(Zaak {
                type_name,
                raw: v,
                summary,
            });
        }
    }
    // Newest aanvraag first.
    out.sort_by(|a, b| {
        b.summary["datumAanvraag"]
            .as_str()
            .cmp(&a.summary["datumAanvraag"].as_str())
    });
    out
}

/// Free texts in a commit that explain a step.
fn toelichtingen(data: &Value) -> Vec<String> {
    let body = data
        .get("patch")
        .or(data.get("resourceData"))
        .cloned()
        .unwrap_or(Value::Null);
    let mut out = Vec::new();
    for key in [
        "aanvullingGevraagd",
        "aanvullingOntvangen",
        "verlengingsreden",
        "intrekkingsreden",
        "heropenreden",
        "besluit",
        "resultaat",
        "toelichting",
    ] {
        match &body[key] {
            Value::String(t) if !t.is_empty() => out.push(t.clone()),
            Value::Object(m) => {
                for k in ["toelichting", "omschrijving", "uitkomst"] {
                    if let Some(t) = m.get(k).and_then(Value::as_str) {
                        out.push(t.to_string());
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// The steps a zaak went through, from its own events.
fn statushistorie(reg: &Register, actor: &Actor, z: &Zaak, gesloten: bool) -> Vec<Value> {
    let sub = subject(&z.raw);
    let mut steps: Vec<(String, Vec<String>)> = Vec::new();
    if z.type_name == ZGW_ZAAK {
        for st in zgw_statussen(reg, actor, sub) {
            let titel = s(&st, "statustoelichting")
                .unwrap_or("Status gezet")
                .to_string();
            steps.push((titel, vec![]));
        }
    } else {
        let events = reg.timeline(actor, sub, 0, 100).unwrap_or_default();
        for e in &events {
            let data = &e["data"];
            let st = data["patch"]["status"]
                .as_str()
                .or(data["resourceData"]["status"].as_str());
            let notes = toelichtingen(data);
            match st {
                Some(st) if steps.last().map(|l| l.0.as_str()) != Some(&capitalize(st)) => {
                    steps.push((capitalize(st), notes))
                }
                _ => {
                    if let Some(last) = steps.last_mut() {
                        last.1.extend(notes);
                    }
                }
            }
        }
    }
    let mut planned = Vec::new();
    if !gesloten {
        let next = match z.type_name {
            AANVRAAG => Some("Besloten"),
            MELDING => Some("Afgehandeld"),
            _ => Some("Afgerond"),
        };
        planned.extend(next.filter(|n| !steps.iter().any(|s| s.0 == *n)));
    }
    let n = steps.len();
    let mut out: Vec<Value> = steps
        .into_iter()
        .enumerate()
        .map(|(i, (titel, notes))| {
            let status = if i + 1 == n && !gesloten {
                "lopend"
            } else {
                "voltooid"
            };
            let mut step = json!({ "nummer": i + 1, "titel": titel, "status": status });
            if !notes.is_empty() {
                step["toelichting"] = json!(notes);
            }
            step
        })
        .collect();
    for (i, titel) in planned.into_iter().enumerate() {
        out.push(json!({ "nummer": n + i + 1, "titel": titel, "status": "gepland" }));
    }
    out
}

fn formaat(naam: &str) -> &'static str {
    match naam
        .rsplit('.')
        .next()
        .unwrap_or_default()
        .to_lowercase()
        .as_str()
    {
        "pdf" => "application/pdf",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "dwg" => "image/vnd.dwg",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        _ => "application/octet-stream",
    }
}

fn documenten(z: &Zaak) -> Vec<Value> {
    let sub = subject(&z.raw);
    let doc = |naam: &str, datum: &str, bron: &str| {
        json!({
            "uuid": uuid_of(&format!("{sub}#{naam}")),
            "naam": naam,
            "formaat": formaat(naam),
            "bestandsgrootte": 0,
            "datum": datum,
            "bron": bron,
            "downloadUrl": sub,
        })
    };
    let names = |v: &Value| -> Vec<String> {
        v.as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default()
    };
    let mut out = Vec::new();
    if z.type_name == AANVRAAG {
        let d = zaak_datum(AANVRAAG, &z.raw);
        for n in names(&z.raw["bijlagen"]) {
            out.push(doc(&n, &d, "burger"));
        }
        let d2 = date(s(&z.raw["aanvullingOntvangen"], "tijd")).unwrap_or_else(|| d.clone());
        for n in names(&z.raw["aanvullingOntvangen"]["bijlagen"]) {
            out.push(doc(&n, &d2, "burger"));
        }
        if let Some(d) = date(s(&z.raw, "besloten")) {
            out.push(doc("Besluit.pdf", &d, "organisatie"));
        }
    }
    out
}

/// Gesprekken about the zaak, as contactmomenten (kanaal `vraag`).
fn contactmomenten(reg: &Register, actor: &Actor, z: &Zaak) -> Vec<Value> {
    reg.related(actor, subject(&z.raw))
        .unwrap_or_default()
        .into_iter()
        .filter(|r| r["_meta"]["type"] == GESPREK)
        .map(|g| {
            json!({
                "datum": date(g["_meta"]["created"].as_str()).unwrap_or_default(),
                "kanaal": "vraag",
                "tekst": s(&g, "gespreksonderwerp").unwrap_or("Gesprek"),
            })
        })
        .collect()
}

// ---------- taken ----------

fn mijn_taken(reg: &Register, actor: &Actor) -> Vec<Value> {
    let mut zaken: HashMap<String, Option<Value>> = HashMap::new();
    let mut out: Vec<Value> = all(reg, actor, TAAK)
        .into_iter()
        .filter(|t| betrokken(TAAK, t, actor))
        .map(|t| {
            let sub = subject(&t).to_string();
            let mut taak = json!({
                "uuid": uuid_of(&sub),
                "titel": { "nl": s(&t, "titel").unwrap_or("Taak") },
                "status": match s(&t, "status").unwrap_or("open") {
                    "vervallen" => "geannuleerd",
                    other => other,
                },
            });
            if let Some(tl) = s(&t, "toelichting") {
                taak["toelichting"] = json!({ "nl": tl });
            }
            if let Some(d) = s(&t, "deadline") {
                // A calendar day is the end of that day (MijnTaken convention).
                taak["deadline"] = json!(if d.len() == 10 {
                    format!("{d}T23:59:59+01:00")
                } else {
                    d.to_string()
                });
            }
            let link = match s(&t, "zaak") {
                Some(z) => {
                    let rec = zaken
                        .entry(z.to_string())
                        .or_insert_with(|| reg.get_subject(actor, z).ok())
                        .clone();
                    let nummer = rec
                        .as_ref()
                        .map(|r| zaaknummer(r["_meta"]["type"].as_str().unwrap_or_default(), r))
                        .unwrap_or_default();
                    taak["context"] = json!({ "urn": urn_for(&nummer, z), "canonicalUrl": z });
                    z.to_string()
                }
                None => sub.clone(),
            };
            taak["uitvoering"] = json!({ "canonicalUrl": link });
            taak
        })
        .collect();
    out.sort_by(|a, b| {
        let k = |t: &Value| t["deadline"].as_str().unwrap_or("9999").to_string();
        k(a).cmp(&k(b))
    });
    out
}

// ---------- HTTP ----------

fn problem(status: StatusCode, code: &str, title: &str, detail: &str) -> Response {
    let body = json!({ "type": "about:blank", "code": code, "title": title, "status": status.as_u16(), "detail": detail });
    let mut res = (status, body.to_string()).into_response();
    res.headers_mut().insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static("application/problem+json"),
    );
    res
}

fn with_version(v: Value) -> Response {
    let mut res = Json(v).into_response();
    res.headers_mut()
        .insert("API-Version", header::HeaderValue::from_static("next"));
    res
}

/// One page in the shape of `patterns/pagination`.
fn page(items: Vec<Value>, p: &ListParams, headers: &HeaderMap, path: &str) -> Response {
    let size = p.page_size.unwrap_or(20);
    if size == 0 || size > 100 {
        return problem(
            StatusCode::BAD_REQUEST,
            "ONGELDIGE_AANVRAAG",
            "Ongeldige aanvraag",
            "Parameter 'pageSize' moet tussen 1 en 100 liggen.",
        );
    }
    let nr = p.page.unwrap_or(1).max(1);
    let count = items.len();
    let host = headers
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("localhost");
    let link = |n: usize| {
        let mut q = format!("page={n}&pageSize={size}");
        if let Some(s) = &p.status {
            q.push_str(&format!("&status={s}"));
        }
        json!(format!("http://{host}{path}?{q}"))
    };
    let results: Vec<Value> = items.into_iter().skip((nr - 1) * size).take(size).collect();
    with_version(json!({
        "count": count,
        "next": if nr * size < count { link(nr + 1) } else { Value::Null },
        "previous": if nr > 1 { link(nr - 1) } else { Value::Null },
        "results": results,
    }))
}

async fn who(state: &AppState, headers: &HeaderMap) -> Result<Actor, Response> {
    actor(state, headers).await.map_err(|_| {
        problem(
            StatusCode::UNAUTHORIZED,
            "NIET_GEAUTHENTICEERD",
            "Authenticatie vereist",
            "Geen geldige toegangstoken meegestuurd.",
        )
    })
}

pub async fn list_zaken(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(p): Query<ListParams>,
) -> Response {
    let actor = match who(&state, &headers).await {
        Ok(a) => a,
        Err(r) => return r,
    };
    let items: Vec<Value> = mijn_zaken(&state.register, &actor)
        .into_iter()
        .map(|z| z.summary)
        .filter(|z| p.status.as_deref().is_none_or(|s| z["status"] == s))
        .collect();
    page(items, &p, &headers, "/mijn/zaken/zaken")
}

pub async fn get_zaak(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(uuid): Path<String>,
) -> Response {
    let actor = match who(&state, &headers).await {
        Ok(a) => a,
        Err(r) => return r,
    };
    let reg = &state.register;
    // Someone else's zaak looks the same as a missing one, so callers cannot probe.
    let Some(z) = mijn_zaken(reg, &actor)
        .into_iter()
        .find(|z| z.summary["uuid"] == uuid.as_str())
    else {
        return problem(
            StatusCode::NOT_FOUND,
            "ZAAK_NIET_GEVONDEN",
            "Zaak niet gevonden",
            "Er bestaat geen zaak met dit uuid.",
        );
    };
    let gesloten = z.summary["status"] == "gesloten";
    let mut detail: Map<String, Value> = z.summary.as_object().cloned().unwrap_or_default();
    detail.insert(
        "statushistorie".into(),
        json!(statushistorie(reg, &actor, &z, gesloten)),
    );
    detail.insert("documenten".into(), json!(documenten(&z)));
    detail.insert(
        "contactmomenten".into(),
        json!(contactmomenten(reg, &actor, &z)),
    );
    with_version(Value::Object(detail))
}

pub async fn list_taken(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(p): Query<ListParams>,
) -> Response {
    let actor = match who(&state, &headers).await {
        Ok(a) => a,
        Err(r) => return r,
    };
    let items: Vec<Value> = mijn_taken(&state.register, &actor)
        .into_iter()
        .filter(|t| p.status.as_deref().is_none_or(|s| t["status"] == s))
        .filter(|t| {
            p.context
                .as_deref()
                .is_none_or(|c| t["context"]["urn"] == c)
        })
        .collect();
    page(items, &p, &headers, "/mijn/taken/taken")
}

pub async fn get_taak(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(uuid): Path<String>,
) -> Response {
    let actor = match who(&state, &headers).await {
        Ok(a) => a,
        Err(r) => return r,
    };
    match mijn_taken(&state.register, &actor)
        .into_iter()
        .find(|t| t["uuid"] == uuid.as_str())
    {
        Some(t) => with_version(t),
        None => problem(
            StatusCode::NOT_FOUND,
            "TAAK_NIET_GEVONDEN",
            "Taak niet gevonden",
            "Er bestaat geen taak met dit uuid.",
        ),
    }
}

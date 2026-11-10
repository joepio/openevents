//! The register: log, idempotency, JSON Commit, projections, rules and reads.
//!
//! OpenEvents is the read side of a CQRS architecture (docs/architectuur.md). Writes
//! are facts; the register only checks what keeps the log trustworthy: the schema,
//! the write rule of the profile, idempotency and `previousCommit`.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::{broadcast, Mutex};

use crate::cql2::{self, ActorContext, Expr};
use crate::kv::{prefix_end, KvError, KvStore, Op};
use crate::profile::{self, Profile, Right, SubjectType, TemplateContext};

pub const TYPE_COMMIT: &str = "json.commit";
pub const TYPE_TRANSACTION: &str = "json.commit.transaction";
pub const MAX_COMMITS: usize = 100;
pub const MAX_PAGE: usize = 100;

/// Who is calling: an id (URI) from the token, its claims, and whether it is a
/// register administrator (may register profiles and bypasses rules).
#[derive(Debug, Clone)]
pub struct Actor {
    pub id: String,
    pub claims: Value,
    pub admin: bool,
}

impl Actor {
    fn ctx(&self) -> ActorContext {
        ActorContext {
            id: self.id.clone(),
            claims: self.claims.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct InvalidParam {
    pub name: String,
    pub code: String,
    pub reason: String,
}

fn ip(name: impl Into<String>, code: &str, reason: impl Into<String>) -> InvalidParam {
    InvalidParam {
        name: name.into(),
        code: code.into(),
        reason: reason.into(),
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RegisterError {
    #[error("invalid request")]
    Invalid(String, Vec<InvalidParam>),
    #[error("forbidden")]
    Forbidden,
    #[error("not found")]
    NotFound,
    #[error("precondition failed")]
    Stale(Vec<InvalidParam>),
    #[error("conflict: {0}")]
    Conflict(String),
    #[error(transparent)]
    Storage(#[from] KvError),
}

fn invalid<T>(detail: &str, params: Vec<InvalidParam>) -> Result<T, RegisterError> {
    Err(RegisterError::Invalid(detail.into(), params))
}

/// The stored state of one subject (its projection).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubjectRecord {
    pub subject: String,
    #[serde(rename = "type")]
    pub type_name: String,
    pub state: Value,
    pub head: String,
    pub sequence: u64,
    pub deleted: bool,
    pub created: String,
    pub updated: String,
}

impl SubjectRecord {
    /// The representation returned to clients, optionally limited to some fields.
    pub fn representation(&self) -> Value {
        self.representation_with(None)
    }

    pub fn representation_with(&self, fields: Option<&[String]>) -> Value {
        let mut out = match (&self.state, fields) {
            (state, Some(f)) => pick_fields(state, f),
            (Value::Object(m), None) => Value::Object(m.clone()),
            _ => json!({}),
        };
        out["_meta"] = json!({
            "subject": self.subject,
            "type": self.type_name,
            "head": self.head,
            "sequence": self.sequence.to_string(),
            "created": self.created,
            "updated": self.updated,
        });
        if fields.is_some() {
            out["_meta"]["limited"] = json!(true);
        }
        out
    }
}

#[derive(Debug, Clone)]
pub struct StoredEvent {
    pub sequence: u64,
    pub subject: String,
    pub event: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct IdempotencyRecord {
    actor: String,
    body: String,
    response: Value,
}

pub struct SubmitOk {
    pub replayed: bool,
    pub body: Value,
}

#[derive(Debug, Default, Clone)]
pub struct EventFilter {
    pub type_prefix: Option<String>,
    pub subject: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueryRequest {
    #[serde(rename = "type")]
    pub type_name: String,
    #[serde(default)]
    pub filter: Option<Value>,
    #[serde(default)]
    pub fields: Option<Vec<String>>,
    #[serde(default)]
    pub page_size: Option<usize>,
    #[serde(default)]
    pub cursor: Option<String>,
}

// ---------- keys ----------

fn seq_key(seq: u64) -> String {
    format!("{seq:020}")
}
fn k_log(seq: u64) -> Vec<u8> {
    format!("log/{}", seq_key(seq)).into_bytes()
}
fn k_subjlog(subject: &str, seq: u64) -> Vec<u8> {
    format!("subjlog/{subject}\n{}", seq_key(seq)).into_bytes()
}
fn k_subjlog_prefix(subject: &str) -> Vec<u8> {
    format!("subjlog/{subject}\n").into_bytes()
}
fn k_state(subject: &str) -> Vec<u8> {
    format!("state/{subject}").into_bytes()
}
fn k_type_index(type_name: &str, subject: &str) -> Vec<u8> {
    format!("type/{type_name}\n{subject}").into_bytes()
}
fn k_type_prefix(type_name: &str) -> Vec<u8> {
    format!("type/{type_name}\n").into_bytes()
}
fn k_idem(source: &str, id: &str) -> Vec<u8> {
    format!("idem/{source}\n{id}").into_bytes()
}
fn k_profile(id: &str) -> Vec<u8> {
    format!("profile/{id}").into_bytes()
}
const K_SEQ: &[u8] = b"meta/seq";

#[derive(Default)]
struct Profiles {
    by_id: HashMap<String, Arc<Profile>>,
    type_owner: HashMap<String, String>,
    event_owner: HashMap<String, String>,
}

impl Profiles {
    fn subject_type<'a>(&self, name: &'a str) -> Option<(Arc<Profile>, &'a str)> {
        let pid = self.type_owner.get(name)?;
        let p = self.by_id.get(pid)?.clone();
        Some((p, name))
    }
    fn event_type(&self, name: &str) -> Option<Arc<Profile>> {
        let pid = self.event_owner.get(name)?;
        self.by_id.get(pid).cloned()
    }
}

pub struct Register {
    kv: Arc<dyn KvStore>,
    profiles: RwLock<Profiles>,
    write_lock: Mutex<()>,
    tx: broadcast::Sender<Arc<StoredEvent>>,
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn parse_json<T: for<'de> Deserialize<'de>>(bytes: &[u8]) -> Result<T, RegisterError> {
    serde_json::from_slice(bytes)
        .map_err(|e| RegisterError::Storage(KvError(format!("corrupt record: {e}"))))
}

/// May this actor use this right on a subject in this state?
fn rule_allows(rule: &Option<Expr>, state: &Value, actor: &Actor) -> bool {
    rule.as_ref()
        .map(|r| r.eval(state, &actor.ctx()))
        .unwrap_or(false)
}

fn readable_by(t: &SubjectType, record: &SubjectRecord, actor: &Actor) -> bool {
    if actor.admin {
        return true;
    }
    if record.deleted {
        return false;
    }
    let s = &record.state;
    rule_allows(&t.rules.read, s, actor)
        || rule_allows(&t.rules.write, s, actor)
        || rule_allows(&t.rules.append, s, actor)
}

/// The fields this actor may see of this subject, or `None` for all fields.
fn visible_fields<'a>(t: &'a SubjectType, state: &Value, actor: &Actor) -> Option<&'a [String]> {
    if actor.admin {
        return None;
    }
    let ctx = actor.ctx();
    t.fields
        .iter()
        .find(|f| f.when.eval(state, &ctx))
        .map(|f| f.show.as_slice())
}

fn pick_fields(state: &Value, fields: &[String]) -> Value {
    let mut out = json!({});
    for f in fields {
        let mut src = state;
        let mut ok = true;
        for part in f.split('.') {
            match src.get(part) {
                Some(v) => src = v,
                None => {
                    ok = false;
                    break;
                }
            }
        }
        if ok {
            let mut dst = &mut out;
            let parts: Vec<&str> = f.split('.').collect();
            for (i, part) in parts.iter().enumerate() {
                if i == parts.len() - 1 {
                    dst[*part] = src.clone();
                } else {
                    if !dst.get(*part).map(|v| v.is_object()).unwrap_or(false) {
                        dst[*part] = json!({});
                    }
                    dst = dst.get_mut(*part).unwrap();
                }
            }
        }
    }
    out
}

/// One change inside a submitted request, before it is stored.
struct Change {
    subject: String,
    event: Value,
    record: SubjectRecord,
    new_subject: bool,
}

impl Register {
    pub fn open(kv: Arc<dyn KvStore>) -> Result<Self, RegisterError> {
        let (tx, _) = broadcast::channel(1024);
        let reg = Register {
            kv,
            profiles: RwLock::new(Profiles::default()),
            write_lock: Mutex::new(()),
            tx,
        };
        let stored = reg
            .kv
            .range(b"profile/", &prefix_end(b"profile/"), usize::MAX)?;
        for (_, v) in stored {
            let raw: Value = parse_json(&v)?;
            let p = Profile::compile(raw)
                .map_err(|e| RegisterError::Storage(KvError(e.to_string())))?;
            reg.install(p);
        }
        Ok(reg)
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Arc<StoredEvent>> {
        self.tx.subscribe()
    }

    pub fn head(&self) -> Result<u64, RegisterError> {
        Ok(match self.kv.get(K_SEQ)? {
            Some(b) => String::from_utf8_lossy(&b).parse().unwrap_or(0),
            None => 0,
        })
    }

    // ---------- profiles ----------

    fn install(&self, p: Profile) {
        let mut ps = self.profiles.write().unwrap();
        if let Some(old) = ps.by_id.remove(&p.id) {
            for t in old.types.keys() {
                ps.type_owner.remove(t);
            }
            for e in old.events.keys() {
                ps.event_owner.remove(e);
            }
        }
        for t in p.types.keys() {
            ps.type_owner.insert(t.clone(), p.id.clone());
        }
        for e in p.events.keys() {
            ps.event_owner.insert(e.clone(), p.id.clone());
        }
        ps.by_id.insert(p.id.clone(), Arc::new(p));
    }

    pub fn put_profile(&self, actor: &Actor, id: &str, raw: Value) -> Result<Value, RegisterError> {
        if !actor.admin {
            return Err(RegisterError::Forbidden);
        }
        let p = Profile::compile(raw.clone())
            .map_err(|e| RegisterError::Invalid(e.to_string(), vec![]))?;
        if p.id != id {
            return invalid(
                "The id in the body must match the id in the path.",
                vec![ip("/id", "mismatch", "id differs from path")],
            );
        }
        {
            let ps = self.profiles.read().unwrap();
            for t in p.types.keys() {
                if let Some(owner) = ps.type_owner.get(t) {
                    if owner != id {
                        return invalid(
                            "Type is owned by another profile.",
                            vec![ip(
                                format!("/types/{t}"),
                                "taken",
                                format!("owned by {owner}"),
                            )],
                        );
                    }
                }
            }
            for e in p.events.keys() {
                if let Some(owner) = ps.event_owner.get(e) {
                    if owner != id {
                        return invalid(
                            "Event type is owned by another profile.",
                            vec![ip(
                                format!("/events/{e}"),
                                "taken",
                                format!("owned by {owner}"),
                            )],
                        );
                    }
                }
            }
        }
        self.kv.apply_batch(vec![Op::Put(
            k_profile(id),
            serde_json::to_vec(&raw).unwrap(),
        )])?;
        self.install(p);
        Ok(raw)
    }

    pub fn get_profile(&self, id: &str) -> Option<Value> {
        self.profiles
            .read()
            .unwrap()
            .by_id
            .get(id)
            .map(|p| p.raw.clone())
    }

    /// Is this event type registered in one of the profiles?
    pub fn knows_event_type(&self, name: &str) -> bool {
        self.profiles.read().unwrap().event_type(name).is_some()
    }

    pub fn list_profiles(&self) -> Vec<Value> {
        let ps = self.profiles.read().unwrap();
        let mut list: Vec<_> = ps
            .by_id
            .values()
            .map(|p| json!({ "id": p.id, "version": p.version, "types": p.types.keys().collect::<Vec<_>>(), "events": p.events.keys().collect::<Vec<_>>() }))
            .collect();
        list.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
        list
    }

    // ---------- reads ----------

    pub fn load_record(&self, subject: &str) -> Result<Option<SubjectRecord>, RegisterError> {
        match self.kv.get(&k_state(subject))? {
            Some(b) => Ok(Some(parse_json(&b)?)),
            None => Ok(None),
        }
    }

    fn readable(&self, record: &SubjectRecord, actor: &Actor) -> bool {
        let ps = self.profiles.read().unwrap();
        match ps.subject_type(&record.type_name) {
            Some((p, name)) => readable_by(&p.types[name], record, actor),
            None => actor.admin,
        }
    }

    /// Whether this actor may currently read events of this subject.
    pub fn may_read_subject(&self, subject: &str, actor: &Actor) -> Result<bool, RegisterError> {
        Ok(match self.load_record(subject)? {
            Some(r) => self.readable(&r, actor),
            None => actor.admin,
        })
    }

    /// The projection of one subject. Unknown and unreadable look the same (404).
    pub fn get_subject(&self, actor: &Actor, subject: &str) -> Result<Value, RegisterError> {
        let Some(r) = self.load_record(subject)? else {
            return Err(RegisterError::NotFound);
        };
        let ps = self.profiles.read().unwrap();
        match ps.subject_type(&r.type_name) {
            Some((p, name)) if readable_by(&p.types[name], &r, actor) => {
                Ok(r.representation_with(visible_fields(&p.types[name], &r.state, actor)))
            }
            None if actor.admin => Ok(r.representation()),
            _ => Err(RegisterError::NotFound),
        }
    }

    fn read_event(&self, seq: u64) -> Result<Option<Value>, RegisterError> {
        match self.kv.get(&k_log(seq))? {
            Some(b) => Ok(Some(parse_json(&b)?)),
            None => Ok(None),
        }
    }

    /// The timeline of one subject, after a sequence number.
    pub fn timeline(
        &self,
        actor: &Actor,
        subject: &str,
        after: u64,
        limit: usize,
    ) -> Result<Vec<Value>, RegisterError> {
        if !self.may_read_subject(subject, actor)? {
            return Err(RegisterError::NotFound);
        }
        let prefix = k_subjlog_prefix(subject);
        let mut start = prefix.clone();
        start.extend_from_slice(seq_key(after + 1).as_bytes());
        let rows = self
            .kv
            .range(&start, &prefix_end(&prefix), limit.min(MAX_PAGE))?;
        let mut out = Vec::new();
        let mut cache = HashMap::new();
        for (k, _) in rows {
            let seq: u64 = String::from_utf8_lossy(&k[prefix.len()..])
                .parse()
                .unwrap_or(0);
            if let Some(e) = self.read_event(seq)? {
                if let Some(e) = self.present_event(&e, actor, &mut cache)? {
                    out.push(e);
                }
            }
        }
        Ok(out)
    }

    /// Events after a sequence number that this actor may read, plus the last sequence
    /// number scanned (the next cursor).
    pub fn list_events(
        &self,
        actor: &Actor,
        after: u64,
        limit: usize,
        filter: &EventFilter,
    ) -> Result<(Vec<Value>, u64), RegisterError> {
        let limit = limit.clamp(1, MAX_PAGE);
        let mut out = Vec::new();
        let mut cursor = after;
        let mut cache: HashMap<String, Visibility> = HashMap::new();
        let end = prefix_end(b"log/");
        loop {
            let rows = self.kv.range(&k_log(cursor + 1), &end, 500)?;
            if rows.is_empty() {
                break;
            }
            for (k, v) in rows {
                let seq: u64 = String::from_utf8_lossy(&k[4..]).parse().unwrap_or(0);
                cursor = seq;
                let event: Value = parse_json(&v)?;
                if !self.event_matches(&event, filter) {
                    continue;
                }
                if let Some(event) = self.present_event(&event, actor, &mut cache)? {
                    out.push(event);
                    if out.len() >= limit {
                        return Ok((out, cursor));
                    }
                }
            }
        }
        Ok((out, cursor))
    }

    pub fn event_matches(&self, event: &Value, filter: &EventFilter) -> bool {
        if let Some(t) = &filter.type_prefix {
            if !event["type"].as_str().unwrap_or("").starts_with(t.as_str()) {
                return false;
            }
        }
        if let Some(s) = &filter.subject {
            if event["subject"].as_str() != Some(s.as_str()) {
                return false;
            }
        }
        true
    }

    /// The event as this actor may see it: `None` when the subject is not readable,
    /// without `data` when a field rule limits what the actor sees of the subject.
    pub fn present_event(
        &self,
        event: &Value,
        actor: &Actor,
        cache: &mut HashMap<String, Visibility>,
    ) -> Result<Option<Value>, RegisterError> {
        let subject = event["subject"].as_str().unwrap_or("").to_string();
        let vis = match cache.get(&subject) {
            Some(v) => *v,
            None => {
                let v = self.subject_visibility(&subject, actor)?;
                cache.insert(subject, v);
                v
            }
        };
        Ok(match vis {
            Visibility::Hidden => None,
            Visibility::Full => Some(event.clone()),
            Visibility::Limited => {
                let mut e = event.clone();
                if let Some(o) = e.as_object_mut() {
                    o.remove("data");
                    o.insert("dataHidden".into(), json!(true));
                }
                Some(e)
            }
        })
    }

    fn subject_visibility(
        &self,
        subject: &str,
        actor: &Actor,
    ) -> Result<Visibility, RegisterError> {
        let Some(r) = self.load_record(subject)? else {
            return Ok(if actor.admin {
                Visibility::Full
            } else {
                Visibility::Hidden
            });
        };
        let ps = self.profiles.read().unwrap();
        Ok(match ps.subject_type(&r.type_name) {
            Some((p, name)) => {
                let t = &p.types[name];
                if !readable_by(t, &r, actor) {
                    Visibility::Hidden
                } else if visible_fields(t, &r.state, actor).is_some() {
                    Visibility::Limited
                } else {
                    Visibility::Full
                }
            }
            None if actor.admin => Visibility::Full,
            None => Visibility::Hidden,
        })
    }

    /// `POST /query`: subjects of one type that match the filter AND the read rule.
    /// First version: scans the type index; no field indexes yet.
    pub fn query(&self, actor: &Actor, req: &QueryRequest) -> Result<Value, RegisterError> {
        let feed_cursor = self.head()?;
        let (profile, type_name) = {
            let ps = self.profiles.read().unwrap();
            match ps.subject_type(&req.type_name) {
                Some((p, n)) => (p, n.to_string()),
                None => {
                    return invalid(
                        "Unknown type.",
                        vec![ip(
                            "/type",
                            "unknown",
                            format!("{} is not a registered type", req.type_name),
                        )],
                    )
                }
            }
        };
        let t = &profile.types[&type_name];
        let filter = match &req.filter {
            None | Some(Value::Null) => None,
            Some(Value::String(t)) if t.trim().is_empty() => None,
            Some(f) => {
                let e = cql2::parse_any(f).map_err(|e| {
                    RegisterError::Invalid(
                        "Invalid filter.".into(),
                        vec![ip("/filter", "invalid", e.0)],
                    )
                })?;
                if e.uses_actor() {
                    return invalid(
                        "actor() is only allowed in profile rules.",
                        vec![ip(
                            "/filter",
                            "actor-not-allowed",
                            "use the read rule of the profile instead",
                        )],
                    );
                }
                // Filtering on a field you may not see gives the same error as an unknown field.
                let hidden: Option<&[String]> = visible_fields(t, &json!({}), actor);
                if let Some(show) = hidden {
                    let bad: Vec<_> = e
                        .properties()
                        .into_iter()
                        .filter(|p| {
                            !show
                                .iter()
                                .any(|s| p == s || p.starts_with(&format!("{s}.")))
                        })
                        .collect();
                    if !bad.is_empty() {
                        return invalid(
                            "Field is not queryable.",
                            bad.into_iter()
                                .map(|p| ip("/filter", "not-queryable", p))
                                .collect(),
                        );
                    }
                }
                if let Some(q) = &t.queryable {
                    let bad: Vec<_> = e
                        .properties()
                        .into_iter()
                        .filter(|p| !q.contains(p))
                        .collect();
                    if !bad.is_empty() {
                        return invalid(
                            "Field is not queryable.",
                            bad.into_iter()
                                .map(|p| ip("/filter", "not-queryable", p))
                                .collect(),
                        );
                    }
                }
                Some(e)
            }
        };
        let page = req.page_size.unwrap_or(20).clamp(1, MAX_PAGE);
        let prefix = k_type_prefix(&type_name);
        let mut start = prefix.clone();
        if let Some(c) = &req.cursor {
            start.extend_from_slice(c.as_bytes());
            start.push(0);
        }
        let end = prefix_end(&prefix);
        let ctx = actor.ctx();
        let mut results = Vec::new();
        let mut next = None;
        'scan: loop {
            let rows = self.kv.range(&start, &end, 500)?;
            if rows.is_empty() {
                break;
            }
            for (k, _) in &rows {
                let subject = String::from_utf8_lossy(&k[prefix.len()..]).to_string();
                start = k.clone();
                start.push(0);
                let Some(record) = self.load_record(&subject)? else {
                    continue;
                };
                if record.deleted || !readable_by(t, &record, actor) {
                    continue;
                }
                let allowed = visible_fields(t, &record.state, actor);
                if let Some(f) = &filter {
                    // You filter over what you may see: for a limited subject the filter
                    // only sees the shown fields, so it cannot probe hidden ones.
                    let matches = match allowed {
                        Some(show) => f.eval(&pick_fields(&record.state, show), &ctx),
                        None => f.eval(&record.state, &ctx),
                    };
                    if !matches {
                        continue;
                    }
                }
                if results.len() == page {
                    next = Some(subject_cursor(&results));
                    break 'scan;
                }
                let mut rep = record.representation_with(allowed);
                if let Some(fields) = &req.fields {
                    let meta = rep["_meta"].clone();
                    rep = pick_fields(&rep, fields);
                    rep["_meta"] = meta;
                }
                results.push(rep);
            }
        }
        Ok(json!({ "results": results, "next": next, "feedCursor": feed_cursor.to_string() }))
    }

    // ---------- writes ----------

    /// `POST /events`: a CloudEvent of a profile event type, a `json.commit` or a
    /// `json.commit.transaction`. All or nothing.
    pub async fn submit(&self, actor: &Actor, envelope: Value) -> Result<SubmitOk, RegisterError> {
        let body = envelope.to_string();
        let obj = match envelope.as_object() {
            Some(o) => o,
            None => {
                return invalid(
                    "Body must be a CloudEvent object.",
                    vec![ip("/", "type", "expected an object")],
                )
            }
        };
        let mut errors = Vec::new();
        for f in ["specversion", "id", "source", "type", "subject"] {
            if obj
                .get(f)
                .and_then(|v| v.as_str())
                .map(|s| s.is_empty())
                .unwrap_or(true)
            {
                errors.push(ip(format!("/{f}"), "required", format!("{f} is required")));
            }
        }
        if obj
            .get("specversion")
            .and_then(|v| v.as_str())
            .is_some_and(|v| v != "1.0")
        {
            errors.push(ip("/specversion", "unsupported", "only CloudEvents 1.0"));
        }
        if !errors.is_empty() {
            return invalid("Not a valid CloudEvent.", errors);
        }
        let id = obj["id"].as_str().unwrap().to_string();
        let source = obj["source"].as_str().unwrap().to_string();
        let event_type = obj["type"].as_str().unwrap().to_string();
        let subject = obj["subject"].as_str().unwrap().to_string();

        let _guard = self.write_lock.lock().await;

        // Idempotency on source + id (CloudEvents uniqueness), bound to the caller.
        if let Some(b) = self.kv.get(&k_idem(&source, &id))? {
            let rec: IdempotencyRecord = parse_json(&b)?;
            if rec.actor == actor.id && rec.body == body {
                return Ok(SubmitOk {
                    replayed: true,
                    body: rec.response,
                });
            }
            return Err(RegisterError::Conflict(
                "An event with this source and id was already processed. Use a new id.".into(),
            ));
        }

        let time = obj
            .get("time")
            .and_then(|t| t.as_str())
            .map(String::from)
            .unwrap_or_else(now);
        let mut overlay: HashMap<String, Option<SubjectRecord>> = HashMap::new();
        let mut changes: Vec<Change> = Vec::new();
        let mut stale = Vec::new();
        let mut errors = Vec::new();

        if event_type == TYPE_COMMIT || event_type == TYPE_TRANSACTION {
            let commits: Vec<(String, Value)> = if event_type == TYPE_TRANSACTION {
                match obj
                    .get("data")
                    .and_then(|d| d.get("commits"))
                    .and_then(|c| c.as_array())
                {
                    Some(list) if !list.is_empty() && list.len() <= MAX_COMMITS => list
                        .iter()
                        .enumerate()
                        .map(|(i, c)| (format!("/data/commits/{i}"), c.clone()))
                        .collect(),
                    _ => {
                        return invalid(
                            "A json.commit.transaction needs 1 to 100 commits in data.commits.",
                            vec![ip(
                                "/data/commits",
                                "required",
                                "non-empty list, at most 100",
                            )],
                        )
                    }
                }
            } else {
                match obj.get("data") {
                    Some(d) if d.is_object() => vec![("/data".into(), d.clone())],
                    _ => {
                        return invalid(
                            "A json.commit needs a JSONCommit in data.",
                            vec![ip("/data", "required", "object expected")],
                        )
                    }
                }
            };
            let in_tx = event_type == TYPE_TRANSACTION;
            for (i, (ptr, commit)) in commits.into_iter().enumerate() {
                let commit_id = if in_tx {
                    format!("{id}:{i}")
                } else {
                    id.clone()
                };
                match self.apply_commit(
                    actor,
                    &ptr,
                    &commit,
                    &commit_id,
                    &source,
                    &time,
                    &mut overlay,
                )? {
                    Ok(c) => changes.push(c),
                    Err(CommitProblem::Invalid(p)) => errors.extend(p),
                    Err(CommitProblem::Stale(p)) => stale.push(p),
                    Err(CommitProblem::Forbidden) => return Err(RegisterError::Forbidden),
                }
            }
        } else {
            match self.apply_domain_event(
                actor,
                &envelope,
                &event_type,
                &subject,
                &id,
                &time,
                &mut overlay,
            )? {
                Ok(c) => changes.push(c),
                Err(CommitProblem::Invalid(p)) => errors.extend(p),
                Err(CommitProblem::Stale(p)) => stale.push(p),
                Err(CommitProblem::Forbidden) => return Err(RegisterError::Forbidden),
            }
        }
        if !errors.is_empty() {
            return invalid("Rejected, nothing stored.", errors);
        }
        if !stale.is_empty() {
            return Err(RegisterError::Stale(stale));
        }

        // Assign sequence numbers and write everything in one batch.
        let mut seq = self.head()?;
        let mut ops = Vec::new();
        let mut stored = Vec::new();
        let mut summary = Vec::new();
        let mut final_records: HashMap<String, SubjectRecord> = HashMap::new();
        for mut c in changes {
            seq += 1;
            c.event["sequence"] = json!(seq.to_string());
            c.event["sequencetype"] = json!("Integer");
            c.record.sequence = seq;
            ops.push(Op::Put(k_log(seq), c.event.to_string().into_bytes()));
            ops.push(Op::Put(k_subjlog(&c.subject, seq), vec![]));
            if c.new_subject {
                ops.push(Op::Put(
                    k_type_index(&c.record.type_name, &c.subject),
                    vec![],
                ));
            }
            summary.push(
                json!({ "id": c.event["id"], "subject": c.subject, "sequence": seq.to_string() }),
            );
            stored.push(Arc::new(StoredEvent {
                sequence: seq,
                subject: c.subject.clone(),
                event: c.event,
            }));
            final_records.insert(c.subject.clone(), c.record);
        }
        for (subject, record) in &final_records {
            ops.push(Op::Put(
                k_state(subject),
                serde_json::to_vec(record).unwrap(),
            ));
        }
        ops.push(Op::Put(K_SEQ.to_vec(), seq.to_string().into_bytes()));
        let response =
            json!({ "transactionId": id, "sequence": seq.to_string(), "events": summary });
        let idem = IdempotencyRecord {
            actor: actor.id.clone(),
            body,
            response: response.clone(),
        };
        ops.push(Op::Put(
            k_idem(&source, &id),
            serde_json::to_vec(&idem).unwrap(),
        ));
        self.kv.apply_batch(ops)?;
        for e in stored {
            let _ = self.tx.send(e);
        }
        Ok(SubmitOk {
            replayed: false,
            body: response,
        })
    }

    fn current(
        &self,
        subject: &str,
        overlay: &mut HashMap<String, Option<SubjectRecord>>,
    ) -> Result<Option<SubjectRecord>, RegisterError> {
        if let Some(r) = overlay.get(subject) {
            return Ok(r.clone());
        }
        let r = self.load_record(subject)?;
        overlay.insert(subject.to_string(), r.clone());
        Ok(r)
    }

    #[allow(clippy::too_many_arguments)]
    fn check_write(
        &self,
        t: &SubjectType,
        before: Option<&SubjectRecord>,
        after_state: &Value,
        right: Right,
        actor: &Actor,
    ) -> bool {
        if actor.admin {
            return true;
        }
        match before {
            Some(b) if !b.deleted => {
                rule_allows(&t.rules.write, &b.state, actor)
                    || (right == Right::Append && rule_allows(&t.rules.append, &b.state, actor))
            }
            _ => {
                let r = if t.rules.create.is_some() {
                    &t.rules.create
                } else {
                    &t.rules.write
                };
                rule_allows(r, after_state, actor)
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn apply_commit(
        &self,
        actor: &Actor,
        ptr: &str,
        commit: &Value,
        commit_id: &str,
        source: &str,
        time: &str,
        overlay: &mut HashMap<String, Option<SubjectRecord>>,
    ) -> Result<Result<Change, CommitProblem>, RegisterError> {
        let bad = |code: &str, reason: String| {
            Ok(Err(CommitProblem::Invalid(vec![ip(ptr, code, reason)])))
        };
        let Some(c) = commit.as_object() else {
            return bad("invalid", "a commit must be an object".into());
        };
        let Some(resource_id) = c
            .get("resourceId")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        else {
            return bad("required", "resourceId is required".into());
        };
        let resource_data = c.get("resourceData");
        let patch = c.get("patch");
        let deleted = c.get("deleted").and_then(|v| v.as_bool()) == Some(true);
        if [resource_data.is_some(), patch.is_some(), deleted]
            .iter()
            .filter(|b| **b)
            .count()
            != 1
        {
            return bad(
                "one-operation",
                "a commit needs exactly one of resourceData, patch or deleted: true".into(),
            );
        }
        let before = self.current(resource_id, overlay)?;
        let live_before = before.as_ref().filter(|b| !b.deleted);

        // Type: from the stored subject, or from `type` / `schema` for a new one.
        let declared = c
            .get("type")
            .or_else(|| c.get("schema"))
            .and_then(|v| v.as_str());
        let ps = self.profiles.read().unwrap();
        let type_name = match (live_before, declared) {
            (Some(b), Some(d)) if !type_matches(&b.type_name, d) => {
                return bad(
                    "type-mismatch",
                    format!("{resource_id} is a {}", b.type_name),
                );
            }
            (Some(b), _) => b.type_name.clone(),
            (None, Some(d)) => match ps.type_owner.keys().find(|t| type_matches(t, d)) {
                Some(t) => t.clone(),
                None => return bad("unknown-type", format!("{d} is not a registered type")),
            },
            (None, None) => {
                return bad(
                    "required",
                    "type (or schema) is required for a new resource".into(),
                )
            }
        };
        let (profile, _) = ps.subject_type(&type_name).expect("type is registered");
        drop(ps);
        let t = &profile.types[&type_name];
        if !t.allow_commits {
            return bad(
                "commits-not-allowed",
                format!("{type_name} only accepts the event types of its profile"),
            );
        }

        if let Some(expected) = c.get("previousCommit").and_then(|v| v.as_str()) {
            let head = live_before.map(|b| b.head.as_str()).unwrap_or("none");
            if head != expected {
                return Ok(Err(CommitProblem::Stale(ip(
                    format!("{ptr}/previousCommit"),
                    "stale",
                    format!("{resource_id} has changed; the last commit is {head}"),
                ))));
            }
        }

        let new_state = if deleted {
            if live_before.is_none() {
                return bad("not-found", format!("{resource_id} does not exist"));
            }
            live_before.unwrap().state.clone()
        } else if let Some(data) = resource_data {
            data.clone()
        } else {
            let Some(b) = live_before else {
                return bad(
                    "not-found",
                    format!("{resource_id} does not exist; use resourceData to create it"),
                );
            };
            let mut s = b.state.clone();
            profile::merge_patch(&mut s, patch.unwrap());
            s
        };
        if !new_state.is_object() {
            return bad("invalid", "the resource must be a JSON object".into());
        }
        if !deleted {
            if let Some(schema) = &t.schema {
                let errs = profile::schema_errors(schema, &new_state);
                if !errs.is_empty() {
                    return Ok(Err(CommitProblem::Invalid(
                        errs.into_iter()
                            .map(|(p, r)| ip(format!("{ptr}{p}"), "schema", r))
                            .collect(),
                    )));
                }
            }
        }
        if !self.check_write(t, live_before, &new_state, Right::Write, actor) {
            return Ok(Err(CommitProblem::Forbidden));
        }

        let mut stored_commit = commit.clone();
        stored_commit["actor"] = json!(actor.id);
        let event = json!({
            "specversion": "1.0",
            "id": commit_id,
            "source": source,
            "type": TYPE_COMMIT,
            "subject": resource_id,
            "time": time,
            "datacontenttype": "application/json",
            "actor": actor.id,
            "data": stored_commit,
        });
        let record = SubjectRecord {
            subject: resource_id.to_string(),
            type_name: type_name.clone(),
            state: new_state,
            head: commit_id.to_string(),
            sequence: 0,
            deleted,
            created: live_before
                .map(|b| b.created.clone())
                .unwrap_or_else(|| time.to_string()),
            updated: time.to_string(),
        };
        let new_subject = before.is_none();
        overlay.insert(resource_id.to_string(), Some(record.clone()));
        Ok(Ok(Change {
            subject: resource_id.to_string(),
            event,
            record,
            new_subject,
        }))
    }

    #[allow(clippy::too_many_arguments)]
    fn apply_domain_event(
        &self,
        actor: &Actor,
        envelope: &Value,
        event_type: &str,
        subject: &str,
        id: &str,
        time: &str,
        overlay: &mut HashMap<String, Option<SubjectRecord>>,
    ) -> Result<Result<Change, CommitProblem>, RegisterError> {
        let bad = |ptr: &str, code: &str, reason: String| {
            Ok(Err(CommitProblem::Invalid(vec![ip(ptr, code, reason)])))
        };
        let profile = match self.profiles.read().unwrap().event_type(event_type) {
            Some(p) => p,
            None => {
                return bad(
                    "/type",
                    "unknown-type",
                    format!("{event_type} is not a registered event type"),
                )
            }
        };
        let def = &profile.events[event_type];
        let t = &profile.types[&def.subject_type];
        let before = self.current(subject, overlay)?;
        let live_before = before.as_ref().filter(|b| !b.deleted);
        match (live_before, def.creates) {
            (Some(_), true) => {
                return bad("/subject", "exists", format!("{subject} already exists"))
            }
            (None, false) => {
                return bad("/subject", "not-found", format!("{subject} does not exist"))
            }
            (Some(b), false) if b.type_name != def.subject_type => {
                return bad(
                    "/subject",
                    "type-mismatch",
                    format!("{subject} is a {}, not a {}", b.type_name, def.subject_type),
                )
            }
            _ => {}
        }
        if let Some(expected) = envelope.get("previouscommit").and_then(|v| v.as_str()) {
            let head = live_before.map(|b| b.head.as_str()).unwrap_or("none");
            if head != expected {
                return Ok(Err(CommitProblem::Stale(ip(
                    "/previouscommit",
                    "stale",
                    format!("{subject} has changed; the last event is {head}"),
                ))));
            }
        }
        let data = envelope.get("data").cloned().unwrap_or(Value::Null);
        if let Some(schema) = &def.dataschema {
            let errs = profile::schema_errors(schema, &data);
            if !errs.is_empty() {
                return Ok(Err(CommitProblem::Invalid(
                    errs.into_iter()
                        .map(|(p, r)| ip(format!("/data{p}"), "schema", r))
                        .collect(),
                )));
            }
        }
        let mut state = live_before
            .map(|b| b.state.clone())
            .unwrap_or_else(|| json!({}));
        let ctx = TemplateContext {
            data: &data,
            time,
            actor: &actor.id,
            id,
            subject,
            event_type,
        };
        profile::apply_projection(&mut state, &def.projection, &ctx);
        if let Some(schema) = &t.schema {
            let errs = profile::schema_errors(schema, &state);
            if !errs.is_empty() {
                return Ok(Err(CommitProblem::Invalid(
                    errs.into_iter()
                        .map(|(p, r)| ip(format!("/state{p}"), "schema", r))
                        .collect(),
                )));
            }
        }
        if !self.check_write(t, live_before, &state, def.right, actor) {
            return Ok(Err(CommitProblem::Forbidden));
        }
        let mut event = envelope.clone();
        event["time"] = json!(time);
        event["actor"] = json!(actor.id);
        let record = SubjectRecord {
            subject: subject.to_string(),
            type_name: def.subject_type.clone(),
            state,
            head: id.to_string(),
            sequence: 0,
            deleted: false,
            created: live_before
                .map(|b| b.created.clone())
                .unwrap_or_else(|| time.to_string()),
            updated: time.to_string(),
        };
        let new_subject = before.is_none();
        overlay.insert(subject.to_string(), Some(record.clone()));
        Ok(Ok(Change {
            subject: subject.to_string(),
            event,
            record,
            new_subject,
        }))
    }
}

#[derive(Debug, Clone, Copy)]
pub enum Visibility {
    Hidden,
    Limited,
    Full,
}

enum CommitProblem {
    Invalid(Vec<InvalidParam>),
    Stale(InvalidParam),
    Forbidden,
}

/// A commit's `type` or `schema` names a type exactly, or is a URL ending in its name.
fn type_matches(type_name: &str, declared: &str) -> bool {
    declared == type_name || declared.ends_with(&format!("/{type_name}"))
}

fn subject_cursor(results: &[Value]) -> String {
    results
        .last()
        .and_then(|r| r["_meta"]["subject"].as_str())
        .unwrap_or("")
        .to_string()
}

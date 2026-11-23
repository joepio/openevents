//! The register: log, idempotency, commits, actions, rules and reads.
//!
//! OpenEvents is the read side of a CQRS architecture (docs/architectuur.md). Writes
//! are facts; the register only checks what keeps the log trustworthy: the schema,
//! the action and its rule, idempotency and `previousCommit`. Every event is a commit
//! (ADR-0008); the state of a resource is its commits applied in order.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::{broadcast, Mutex};

use crate::cql2::{self, ActorContext, Expr, Refs};
use crate::kv::{prefix_end, KvError, KvStore, Op};
use crate::profile::{self, Action, CommitOp, CommitSpec, Profile, SubjectType};

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
    action_owner: HashMap<String, String>,
}

impl Profiles {
    fn subject_type<'a>(&self, name: &'a str) -> Option<(Arc<Profile>, &'a str)> {
        let pid = self.type_owner.get(name)?;
        let p = self.by_id.get(pid)?.clone();
        Some((p, name))
    }
    fn action(&self, name: &str) -> Option<Arc<Profile>> {
        let pid = self.action_owner.get(name)?;
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

/// Does this rule hold for this actor on a subject in this state?
fn rule_allows(rule: &Option<Expr>, state: &Value, actor: &Actor, refs: &dyn Refs) -> bool {
    rule.as_ref()
        .map(|r| r.eval_with(state, &actor.ctx(), refs))
        .unwrap_or(false)
}

fn readable_by(t: &SubjectType, record: &SubjectRecord, actor: &Actor, refs: &dyn Refs) -> bool {
    if actor.admin {
        return true;
    }
    if record.deleted {
        return false;
    }
    let s = &record.state;
    rule_allows(&t.rules.read, s, actor, refs) || rule_allows(&t.rules.write, s, actor, refs)
}

/// The fields this actor may see of this subject, or `None` for all fields.
fn visible_fields<'a>(
    t: &'a SubjectType,
    state: &Value,
    actor: &Actor,
    refs: &dyn Refs,
) -> Option<&'a [String]> {
    if actor.admin {
        return None;
    }
    let ctx = actor.ctx();
    t.fields
        .iter()
        .find(|f| f.when.eval_with(state, &ctx, refs))
        .map(|f| f.show.as_slice())
}

/// References as profile rules see them: every live subject, including the ones
/// written earlier in the same request.
struct RuleRefs<'a> {
    reg: &'a Register,
    overlay: Option<&'a HashMap<String, Option<SubjectRecord>>>,
    cache: RefCell<HashMap<String, Option<Value>>>,
}

impl Refs for RuleRefs<'_> {
    fn get(&self, subject: &str) -> Option<Value> {
        if let Some(r) = self.overlay.and_then(|o| o.get(subject)) {
            return r.as_ref().filter(|r| !r.deleted).map(|r| r.state.clone());
        }
        if let Some(v) = self.cache.borrow().get(subject) {
            return v.clone();
        }
        let v = self
            .reg
            .load_record(subject)
            .ok()
            .flatten()
            .filter(|r| !r.deleted)
            .map(|r| r.state);
        self.cache.borrow_mut().insert(subject.into(), v.clone());
        v
    }
}

/// References as a client filter sees them: only subjects this actor may read, and of
/// those only the fields it may see. A filter cannot reach further than a GET could.
struct ReaderRefs<'a> {
    reg: &'a Register,
    actor: &'a Actor,
    cache: RefCell<HashMap<String, Option<Value>>>,
}

impl Refs for ReaderRefs<'_> {
    fn get(&self, subject: &str) -> Option<Value> {
        if let Some(v) = self.cache.borrow().get(subject) {
            return v.clone();
        }
        let v = self.reg.load_record(subject).ok().flatten().and_then(|r| {
            let ps = self.reg.profiles.read().unwrap();
            let (p, name) = ps.subject_type(&r.type_name)?;
            let t = &p.types[name];
            let rules = self.reg.rule_refs(None);
            if !readable_by(t, &r, self.actor, &rules) {
                return None;
            }
            Some(match visible_fields(t, &r.state, self.actor, &rules) {
                Some(show) => pick_fields(&r.state, show),
                None => r.state,
            })
        });
        self.cache.borrow_mut().insert(subject.into(), v.clone());
        v
    }
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
        for (k, v) in stored {
            let raw: Value = parse_json(&v)?;
            // A stored profile in an older form (ADR-0008) is skipped, not fatal: put
            // it again in the new form.
            match Profile::compile(raw) {
                Ok(p) => reg.install(p),
                Err(e) => eprintln!(
                    "openevents: skipping stored profile {}: {e}",
                    String::from_utf8_lossy(&k)
                ),
            }
        }
        Ok(reg)
    }

    fn rule_refs<'a>(
        &'a self,
        overlay: Option<&'a HashMap<String, Option<SubjectRecord>>>,
    ) -> RuleRefs<'a> {
        RuleRefs {
            reg: self,
            overlay,
            cache: RefCell::default(),
        }
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
            for e in old.actions.keys() {
                ps.action_owner.remove(e);
            }
        }
        for t in p.types.keys() {
            ps.type_owner.insert(t.clone(), p.id.clone());
        }
        for e in p.actions.keys() {
            ps.action_owner.insert(e.clone(), p.id.clone());
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
            for e in p.actions.keys() {
                if let Some(owner) = ps.action_owner.get(e) {
                    if owner != id {
                        return invalid(
                            "Action is owned by another profile.",
                            vec![ip(
                                format!("/actions/{e}"),
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

    /// Is this action (event type) registered in one of the profiles?
    pub fn knows_action(&self, name: &str) -> bool {
        self.profiles.read().unwrap().action(name).is_some()
    }

    pub fn list_profiles(&self) -> Vec<Value> {
        let ps = self.profiles.read().unwrap();
        let mut list: Vec<_> = ps
            .by_id
            .values()
            .map(|p| json!({ "id": p.id, "version": p.version, "types": p.types.keys().collect::<Vec<_>>(), "actions": p.actions.keys().collect::<Vec<_>>(),
                "titles": p.actions.iter().filter_map(|(n, a)| a.title.as_ref().map(|t| (n.clone(), json!(t)))).collect::<serde_json::Map<_, _>>(),
                "standalone": p.types.iter().filter(|(_, t)| !t.part_of).map(|(n, _)| n).collect::<Vec<_>>() }))
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
            Some((p, name)) => readable_by(&p.types[name], record, actor, &self.rule_refs(None)),
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
        let refs = self.rule_refs(None);
        match ps.subject_type(&r.type_name) {
            Some((p, name)) if readable_by(&p.types[name], &r, actor, &refs) => {
                Ok(r.representation_with(visible_fields(&p.types[name], &r.state, actor, &refs)))
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

    /// What refers to this subject (one level), as this actor may see it: resources
    /// whose reference field (`references` in the profile) holds the subject's id. A
    /// reference the actor may not filter on is skipped, so this never shows more than
    /// a query could.
    pub fn related(&self, actor: &Actor, subject: &str) -> Result<Vec<Value>, RegisterError> {
        if !self.may_read_subject(subject, actor)? {
            return Err(RegisterError::NotFound);
        }
        let mut refs: Vec<(String, String)> = {
            let ps = self.profiles.read().unwrap();
            ps.by_id
                .values()
                .flat_map(|p| {
                    p.types.iter().flat_map(|(name, t)| {
                        t.references.iter().map(move |r| (name.clone(), r.clone()))
                    })
                })
                .collect()
        };
        refs.sort();
        let mut out = Vec::new();
        for (type_name, field) in refs {
            let req = QueryRequest {
                type_name,
                filter: Some(json!({ "op": "=", "args": [ { "property": field }, subject ] })),
                fields: None,
                page_size: Some(MAX_PAGE),
                cursor: None,
            };
            if let Ok(v) = self.query(actor, &req) {
                if let Some(list) = v["results"].as_array() {
                    out.extend(list.iter().cloned());
                }
            }
        }
        Ok(out)
    }

    /// The audit trail of a subject: its own events and those of everything that
    /// refers to it, two levels deep (a zaak, its gesprekken, their bijdragen), in log
    /// order. Read rules and field rules apply to every event, as on the stream.
    pub fn audit(&self, actor: &Actor, subject: &str) -> Result<Value, RegisterError> {
        if !self.may_read_subject(subject, actor)? {
            return Err(RegisterError::NotFound);
        }
        let mut subjects = vec![subject.to_string()];
        let mut frontier = vec![subject.to_string()];
        for _ in 0..2 {
            let mut next = Vec::new();
            for s in &frontier {
                for r in self.related(actor, s)? {
                    if let Some(id) = r["_meta"]["subject"].as_str() {
                        if !subjects.iter().any(|x| x == id) && subjects.len() < 200 {
                            subjects.push(id.to_string());
                            next.push(id.to_string());
                        }
                    }
                }
            }
            frontier = next;
        }
        let mut events = Vec::new();
        for s in &subjects {
            if let Ok(list) = self.timeline(actor, s, 0, MAX_PAGE) {
                events.extend(list);
            }
        }
        let seq = |e: &Value| {
            e["sequence"]
                .as_str()
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(0)
        };
        events.sort_by_key(seq);
        Ok(json!({ "subjects": subjects, "events": events }))
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
                let refs = self.rule_refs(None);
                if !readable_by(t, &r, actor, &refs) {
                    Visibility::Hidden
                } else if visible_fields(t, &r.state, actor, &refs).is_some() {
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
                let hidden: Option<&[String]> = visible_fields(t, &json!({}), actor, &cql2::NoRefs);
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
                        .filter(|p| !q.iter().any(|s| p == s || p.starts_with(&format!("{s}."))))
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
        let rules = self.rule_refs(None);
        let reader = ReaderRefs {
            reg: self,
            actor,
            cache: RefCell::default(),
        };
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
                if record.deleted || !readable_by(t, &record, actor, &rules) {
                    continue;
                }
                let allowed = visible_fields(t, &record.state, actor, &rules);
                if let Some(f) = &filter {
                    // You filter over what you may see: for a limited subject the filter
                    // only sees the shown fields, so it cannot probe hidden ones.
                    let matches = match allowed {
                        Some(show) => f.eval_with(&pick_fields(&record.state, show), &ctx, &reader),
                        None => f.eval_with(&record.state, &ctx, &reader),
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

        let bare = event_type == TYPE_COMMIT || event_type == TYPE_TRANSACTION;
        let profile = if bare {
            None
        } else {
            match self.profiles.read().unwrap().action(&event_type) {
                Some(p) => Some(p),
                None => {
                    return invalid(
                        "Unknown event type.",
                        vec![ip(
                            "/type",
                            "unknown-type",
                            format!("{event_type} is not an action of a registered profile"),
                        )],
                    )
                }
            }
        };
        let action = profile.as_ref().map(|p| &p.actions[&event_type]);
        let data = obj.get("data");
        let list = data.and_then(|d| d.get("commits"));
        let commits: Vec<(String, Value)> = if event_type == TYPE_TRANSACTION || list.is_some() {
            match list.and_then(|c| c.as_array()) {
                Some(list) if !list.is_empty() && list.len() <= MAX_COMMITS => list
                    .iter()
                    .enumerate()
                    .map(|(i, c)| (format!("/data/commits/{i}"), c.clone()))
                    .collect(),
                _ => {
                    return invalid(
                        "data.commits needs 1 to 100 commits.",
                        vec![ip(
                            "/data/commits",
                            "required",
                            "non-empty list, at most 100",
                        )],
                    )
                }
            }
        } else {
            match data {
                Some(Value::Object(d)) => {
                    // One commit: it is about the subject of the event.
                    let mut d = d.clone();
                    d.entry("resourceId")
                        .or_insert_with(|| Value::String(subject.clone()));
                    vec![("/data".into(), Value::Object(d))]
                }
                _ => {
                    return invalid(
                        "data must be a commit (or hold data.commits).",
                        vec![ip("/data", "required", "object expected")],
                    )
                }
            }
        };
        let many = commits.len() > 1 || event_type == TYPE_TRANSACTION;
        for (i, (ptr, commit)) in commits.into_iter().enumerate() {
            let commit_id = if many {
                format!("{id}:{i}")
            } else {
                id.clone()
            };
            let mut event = envelope.clone();
            if let Some(o) = event.as_object_mut() {
                o.remove("data");
                if many {
                    o.insert("transactionid".into(), json!(id));
                }
            }
            event["id"] = json!(commit_id);
            event["time"] = json!(time);
            event["actor"] = json!(actor.id);
            event["datacontenttype"] = json!("application/json");
            if bare {
                event["type"] = json!(TYPE_COMMIT);
            }
            let ctx = CommitCtx {
                ptr: &ptr,
                commit_id: &commit_id,
                time: &time,
                action: profile.as_deref().zip(action),
            };
            match self.apply_commit(actor, &ctx, &commit, event, &mut overlay)? {
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

    fn check_write(
        &self,
        t: &SubjectType,
        before: Option<&SubjectRecord>,
        after_state: &Value,
        actor: &Actor,
        refs: &dyn Refs,
    ) -> bool {
        if actor.admin {
            return true;
        }
        match before {
            Some(b) if !b.deleted => rule_allows(&t.rules.write, &b.state, actor, refs),
            _ => {
                let r = if t.rules.create.is_some() {
                    &t.rules.create
                } else {
                    &t.rules.write
                };
                rule_allows(r, after_state, actor, refs)
            }
        }
    }

    /// Check and apply one commit. With an action, the commit must match one of its
    /// specs; the spec's `set` is added and its rule (or the type's rules) decides.
    /// Without one (a bare `json.commit`), the type must allow commits.
    fn apply_commit(
        &self,
        actor: &Actor,
        ctx: &CommitCtx,
        commit: &Value,
        mut event: Value,
        overlay: &mut HashMap<String, Option<SubjectRecord>>,
    ) -> Result<Result<Change, CommitProblem>, RegisterError> {
        let ptr = ctx.ptr;
        let bad = |at: &str, code: &str, reason: String| {
            Ok(Err(CommitProblem::Invalid(vec![ip(
                format!("{ptr}{at}"),
                code,
                reason,
            )])))
        };
        let Some(c) = commit.as_object() else {
            return bad("", "invalid", "a commit must be an object".into());
        };
        let Some(resource_id) = c
            .get("resourceId")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(String::from)
        else {
            return bad("/resourceId", "required", "resourceId is required".into());
        };
        let deleted = c.get("deleted").and_then(|v| v.as_bool()) == Some(true);
        let op = match (c.get("resourceData"), c.get("patch"), deleted) {
            (Some(_), None, false) => CommitOp::Create,
            (None, Some(_), false) => CommitOp::Update,
            (None, None, true) => CommitOp::Delete,
            _ => {
                return bad(
                    "",
                    "one-operation",
                    "a commit needs exactly one of resourceData, patch or deleted: true".into(),
                )
            }
        };
        let before = self.current(&resource_id, overlay)?;
        let live_before = before.as_ref().filter(|b| !b.deleted).cloned();

        // Type: from the stored resource, or from `type` / `schema` for a new one. In an
        // action with one create, the type may be left out.
        let declared = c
            .get("type")
            .or_else(|| c.get("schema"))
            .and_then(|v| v.as_str())
            .map(String::from)
            .or_else(|| {
                let (_, a) = ctx.action?;
                let mut creates = a.commits.iter().filter(|s| s.op == CommitOp::Create);
                match (creates.next(), creates.next()) {
                    (Some(s), None) if live_before.is_none() => Some(s.type_name.clone()),
                    _ => None,
                }
            });
        let ps = self.profiles.read().unwrap();
        let type_name = match (&live_before, &declared) {
            (Some(b), Some(d)) if !type_matches(&b.type_name, d) => {
                return bad(
                    "/type",
                    "type-mismatch",
                    format!("{resource_id} is a {}", b.type_name),
                );
            }
            (Some(b), _) => b.type_name.clone(),
            (None, Some(d)) => match ps.type_owner.keys().find(|t| type_matches(t, d)) {
                Some(t) => t.clone(),
                None => {
                    return bad(
                        "/type",
                        "unknown-type",
                        format!("{d} is not a registered type"),
                    )
                }
            },
            (None, None) => {
                return bad(
                    "/type",
                    "required",
                    "type (or schema) is required for a new resource".into(),
                )
            }
        };
        let (profile, _) = ps.subject_type(&type_name).expect("type is registered");
        drop(ps);
        let t = &profile.types[&type_name];

        match (op, &live_before) {
            (CommitOp::Create, Some(_)) if ctx.action.is_some() => {
                return bad(
                    "/resourceId",
                    "exists",
                    format!("{resource_id} already exists"),
                )
            }
            (CommitOp::Update | CommitOp::Delete, None) => {
                return bad(
                    "/resourceId",
                    "not-found",
                    format!("{resource_id} does not exist"),
                )
            }
            _ => {}
        }

        let mut commit = c.clone();
        commit.insert("resourceId".into(), json!(resource_id));
        if live_before.is_none() {
            commit.insert("type".into(), json!(type_name));
        }
        let spec: Option<&CommitSpec> = match ctx.action {
            None => {
                if !t.allow_commits {
                    return bad(
                        "",
                        "commits-not-allowed",
                        format!("{type_name} only accepts the actions of its profile"),
                    );
                }
                None
            }
            Some((_, a)) => {
                let Some(spec) = a
                    .commits
                    .iter()
                    .find(|s| s.type_name == type_name && s.op == op)
                else {
                    return bad(
                        "",
                        "not-allowed",
                        format!("{} does not allow a {} of {type_name}", a.name, op.name()),
                    );
                };
                let body_key = match op {
                    CommitOp::Create => Some("resourceData"),
                    CommitOp::Update => Some("patch"),
                    CommitOp::Delete => None,
                };
                if let Some(key) = body_key {
                    let Some(body) = commit.get_mut(key).and_then(|b| b.as_object_mut()) else {
                        return bad(&format!("/{key}"), "invalid", "object expected".into());
                    };
                    if let Some(fields) = &spec.fields {
                        let extra: Vec<String> = body
                            .keys()
                            .filter(|k| !fields.contains(k))
                            .cloned()
                            .collect();
                        if !extra.is_empty() {
                            return Ok(Err(CommitProblem::Invalid(
                                extra
                                    .into_iter()
                                    .map(|k| {
                                        ip(
                                            format!("{ptr}/{key}/{k}"),
                                            "field-not-allowed",
                                            format!("{} does not let you set {k}", a.name),
                                        )
                                    })
                                    .collect(),
                            )));
                        }
                    }
                    for (k, v) in &spec.set {
                        body.insert(k.clone(), profile::render_set(v, &actor.id, ctx.time));
                    }
                }
                Some(spec)
            }
        };

        if let Some(expected) = commit.get("previousCommit").and_then(|v| v.as_str()) {
            let head = live_before
                .as_ref()
                .map(|b| b.head.as_str())
                .unwrap_or("none");
            if head != expected {
                return Ok(Err(CommitProblem::Stale(ip(
                    format!("{ptr}/previousCommit"),
                    "stale",
                    format!("{resource_id} has changed; the last commit is {head}"),
                ))));
            }
        }

        let new_state = match op {
            CommitOp::Delete => live_before.as_ref().unwrap().state.clone(),
            CommitOp::Create => commit["resourceData"].clone(),
            CommitOp::Update => {
                let mut s = live_before.as_ref().unwrap().state.clone();
                profile::merge_patch(&mut s, &commit["patch"]);
                s
            }
        };
        if !new_state.is_object() {
            return bad("", "invalid", "the resource must be a JSON object".into());
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
        let allowed = {
            let refs = self.rule_refs(Some(&*overlay));
            match spec.and_then(|s| s.rule.as_ref()) {
                _ if actor.admin => true,
                // A create is judged on the new resource, a change on what it was.
                Some(rule) => {
                    let on = match &live_before {
                        Some(b) if op != CommitOp::Create => &b.state,
                        _ => &new_state,
                    };
                    rule.eval_with(on, &actor.ctx(), &refs)
                }
                None => self.check_write(t, live_before.as_ref(), &new_state, actor, &refs),
            }
        };
        if !allowed {
            return Ok(Err(CommitProblem::Forbidden));
        }

        event["subject"] = json!(resource_id);
        event["data"] = Value::Object(commit);
        let record = SubjectRecord {
            subject: resource_id.clone(),
            type_name: type_name.clone(),
            state: new_state,
            head: ctx.commit_id.to_string(),
            sequence: 0,
            deleted,
            created: live_before
                .as_ref()
                .map(|b| b.created.clone())
                .unwrap_or_else(|| ctx.time.to_string()),
            updated: ctx.time.to_string(),
        };
        let new_subject = before.is_none();
        overlay.insert(resource_id.clone(), Some(record.clone()));
        Ok(Ok(Change {
            subject: resource_id,
            event,
            record,
            new_subject,
        }))
    }
}

/// Where a commit sits in the request, and the action it belongs to.
struct CommitCtx<'a> {
    ptr: &'a str,
    commit_id: &'a str,
    time: &'a str,
    action: Option<(&'a Profile, &'a Action)>,
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

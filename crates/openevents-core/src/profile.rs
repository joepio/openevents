//! Profiles: a domain on the register, as data (docs/profielen.md).
//!
//! A profile declares types (state schema, queryable fields, read and write rules in
//! CQL2) and actions: named handelingen that say which commits a client may send
//! (ADR-0008). Every event is a commit; its CloudEvent type names the action.

use std::collections::HashMap;

use serde_json::{Map, Value};

use crate::cql2::{self, Expr};

#[derive(Debug, thiserror::Error)]
#[error("invalid profile: {0}")]
pub struct ProfileError(pub String);

fn perr<T>(msg: impl Into<String>) -> Result<T, ProfileError> {
    Err(ProfileError(msg.into()))
}

#[derive(Default)]
pub struct Rules {
    pub read: Option<Expr>,
    pub create: Option<Expr>,
    pub write: Option<Expr>,
    /// Which resources are about the actor ("mijn"): for an overview of their own
    /// zaken, taken and gesprekken. Queries with `mine` and the `mine` attribute on
    /// streamed events use it. Not a permission: reading still needs `read`.
    pub mine: Option<Expr>,
}

/// Which fields an actor sees: the first rule whose `when` holds applies.
pub struct FieldRule {
    pub when: Expr,
    pub show: Vec<String>,
}

pub struct SubjectType {
    pub name: String,
    pub fields: Vec<FieldRule>,
    pub schema: Option<jsonschema::Validator>,
    pub queryable: Option<Vec<String>>,
    /// Fields that hold the id of another resource (its parent, its zaak). Used to
    /// find what refers to a resource, for a zaak overview and its audit trail.
    pub references: Vec<String>,
    /// A type whose reference is required is part of something else (a bijdrage of a
    /// gesprek); the others stand on their own and show up in an overview.
    pub part_of: bool,
    pub allow_commits: bool,
    pub rules: Rules,
    /// Fields with an index: the queryable fields, the references and the own fields
    /// the `mine` rule uses. A query that asks for one of their values reads the index
    /// instead of every resource of the type.
    pub indexed: Vec<String>,
    /// How long a resource is kept: `keep` after the moment in field `from`. After that
    /// the register purges it, with what refers to it (see `Register::purge`).
    pub retention: Option<Retention>,
}

#[derive(Debug, Clone)]
pub struct Retention {
    pub from: String,
    pub months: u32,
    pub days: i64,
    pub seconds: i64,
}

impl Retention {
    /// `P1Y`, `P6M`, `P30D`, `P2W`, `PT12H`, `P1Y6M`: years, months, weeks, days,
    /// hours, minutes, seconds.
    pub fn parse(from: &str, keep: &str) -> Option<Retention> {
        let rest = keep.strip_prefix('P')?;
        let (date, time) = rest.split_once('T').unwrap_or((rest, ""));
        let (mut months, mut days, mut seconds) = (0u32, 0i64, 0i64);
        let mut num = String::new();
        for c in date.chars() {
            if c.is_ascii_digit() {
                num.push(c);
                continue;
            }
            let n: i64 = num.parse().ok()?;
            num.clear();
            match c {
                'Y' => months += (n * 12) as u32,
                'M' => months += n as u32,
                'W' => days += n * 7,
                'D' => days += n,
                _ => return None,
            }
        }
        for c in time.chars() {
            if c.is_ascii_digit() {
                num.push(c);
                continue;
            }
            let n: i64 = num.parse().ok()?;
            num.clear();
            match c {
                'H' => seconds += n * 3600,
                'M' => seconds += n * 60,
                'S' => seconds += n,
                _ => return None,
            }
        }
        if !num.is_empty() || (months == 0 && days == 0 && seconds == 0) {
            return None;
        }
        Some(Retention { from: from.to_string(), months, days, seconds })
    }

    /// When a resource with this state expires, if its `from` field holds a moment.
    pub fn expires(&self, state: &Value) -> Option<chrono::DateTime<chrono::Utc>> {
        let v = state.pointer(&format!("/{}", self.from.replace('.', "/")))?.as_str()?;
        let start = chrono::DateTime::parse_from_rfc3339(v)
            .map(|d| d.with_timezone(&chrono::Utc))
            .ok()
            .or_else(|| {
                chrono::NaiveDate::parse_from_str(v, "%Y-%m-%d")
                    .ok()
                    .map(|d| d.and_hms_opt(0, 0, 0).unwrap().and_utc())
            })?;
        start.checked_add_months(chrono::Months::new(self.months))?
            .checked_add_signed(chrono::Duration::days(self.days) + chrono::Duration::seconds(self.seconds))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommitOp {
    Create,
    Update,
    Delete,
}

impl CommitOp {
    pub fn name(self) -> &'static str {
        match self {
            CommitOp::Create => "create",
            CommitOp::Update => "update",
            CommitOp::Delete => "delete",
        }
    }
}

/// One kind of commit an action allows.
pub struct CommitSpec {
    pub type_name: String,
    pub op: CommitOp,
    /// Top-level fields the client may set; `None` is any field.
    pub fields: Option<Vec<String>>,
    /// Fields the server sets: constants, `$actor` and `$time`.
    pub set: Map<String, Value>,
    /// Who may do this. Without one, the type's create / write rule applies.
    pub rule: Option<Expr>,
}

/// A handeling: a named, authorised set of commits.
pub struct Action {
    pub name: String,
    pub title: Option<String>,
    pub commits: Vec<CommitSpec>,
}

pub struct Profile {
    pub id: String,
    pub version: String,
    pub raw: Value,
    pub types: HashMap<String, SubjectType>,
    pub actions: HashMap<String, Action>,
}

/// Required fields of a schema, also through `allOf` (a profile that extends a shared
/// schema).
fn required_fields(schema: &Value) -> Vec<&str> {
    let mut out: Vec<&str> = schema
        .get("required")
        .and_then(|r| r.as_array())
        .map(|r| r.iter().filter_map(|v| v.as_str()).collect())
        .unwrap_or_default();
    if let Some(all) = schema.get("allOf").and_then(|a| a.as_array()) {
        for s in all {
            out.extend(required_fields(s));
        }
    }
    out
}

fn compile_schema(v: &Value, at: &str) -> Result<jsonschema::Validator, ProfileError> {
    jsonschema::validator_for(v)
        .map_err(|e| ProfileError(format!("{at}: invalid JSON Schema: {e}")))
}

fn rule(obj: &Map<String, Value>, name: &str, at: &str) -> Result<Option<Expr>, ProfileError> {
    match obj.get(name) {
        None => Ok(None),
        Some(v) => cql2::parse_any(v)
            .map(Some)
            .map_err(|e| ProfileError(format!("{at}.rules.{name}: {e}"))),
    }
}

fn str_field<'a>(obj: &'a Map<String, Value>, name: &str) -> Option<&'a str> {
    obj.get(name).and_then(|v| v.as_str())
}

impl Profile {
    pub fn compile(raw: Value) -> Result<Profile, ProfileError> {
        let obj = raw
            .as_object()
            .ok_or_else(|| ProfileError("profile must be an object".into()))?;
        let id = str_field(obj, "id")
            .ok_or_else(|| ProfileError("missing id".into()))?
            .to_string();
        let version = str_field(obj, "version").unwrap_or("0.0.0").to_string();

        let mut types = HashMap::new();
        if let Some(t) = obj.get("types") {
            let t = t
                .as_object()
                .ok_or_else(|| ProfileError("types must be an object".into()))?;
            for (name, def) in t {
                let at = format!("types.{name}");
                let def = def
                    .as_object()
                    .ok_or_else(|| ProfileError(format!("{at} must be an object")))?;
                let rules = match def.get("rules") {
                    None => Rules::default(),
                    Some(r) => {
                        let r = r
                            .as_object()
                            .ok_or_else(|| ProfileError(format!("{at}.rules must be an object")))?;
                        Rules {
                            read: rule(r, "read", &at)?,
                            create: rule(r, "create", &at)?,
                            write: rule(r, "write", &at)?,
                            mine: rule(r, "mine", &at)?,
                        }
                    }
                };
                let mut fields = Vec::new();
                if let Some(list) = def.get("rules").and_then(|r| r.get("fields")) {
                    let list = list
                        .as_array()
                        .ok_or_else(|| ProfileError(format!("{at}.rules.fields must be a list")))?;
                    for (i, f) in list.iter().enumerate() {
                        let fat = format!("{at}.rules.fields[{i}]");
                        let when = f
                            .get("when")
                            .ok_or_else(|| ProfileError(format!("{fat}: missing when")))
                            .and_then(|w| {
                                cql2::parse_any(w)
                                    .map_err(|e| ProfileError(format!("{fat}.when: {e}")))
                            })?;
                        let show = f
                            .get("show")
                            .and_then(|s| s.as_array())
                            .ok_or_else(|| {
                                ProfileError(format!("{fat}: show must be a list of fields"))
                            })?
                            .iter()
                            .filter_map(|v| v.as_str().map(String::from))
                            .collect();
                        fields.push(FieldRule { when, show });
                    }
                }
                let references: Vec<String> = def
                    .get("references")
                    .and_then(|r| r.as_array())
                    .map(|r| {
                        r.iter()
                            .filter_map(|s| s.as_str().map(String::from))
                            .collect()
                    })
                    .unwrap_or_default();
                let required = def.get("schema").map(required_fields).unwrap_or_default();
                let part_of = references.iter().any(|r| required.contains(&r.as_str()));
                let mut queryable: Option<Vec<String>> =
                    def.get("queryable").and_then(|q| q.as_array()).map(|q| {
                        q.iter()
                            .filter_map(|s| s.as_str().map(String::from))
                            .collect()
                    });
                // A reference can always be filtered on.
                if let Some(q) = queryable.as_mut() {
                    for r in &references {
                        if !q.contains(r) {
                            q.push(r.clone());
                        }
                    }
                }
                // Paths through a reference (gesprek.team) cannot be indexed on this type.
                let mut indexed: Vec<String> = queryable.clone().unwrap_or_default();
                indexed.extend(references.iter().cloned());
                if let Some(m) = &rules.mine {
                    indexed.extend(m.properties());
                }
                indexed.retain(|p| {
                    let first = p.split('.').next().unwrap_or("");
                    !(p.contains('.') && references.iter().any(|r| r == first))
                });
                indexed.sort();
                indexed.dedup();
                let retention = match def.get("retention") {
                    None => None,
                    Some(r) => {
                        let from = r.get("from").and_then(|v| v.as_str());
                        let keep = r.get("keep").and_then(|v| v.as_str());
                        Some(
                            from.zip(keep)
                                .and_then(|(f, k)| Retention::parse(f, k))
                                .ok_or_else(|| {
                                    ProfileError(format!(
                                        "{at}.retention: needs from (a field) and keep (an ISO 8601 duration such as P1Y)"
                                    ))
                                })?,
                        )
                    }
                };
                types.insert(
                    name.clone(),
                    SubjectType {
                        retention,
                        indexed,
                        name: name.clone(),
                        fields,
                        references,
                        part_of,
                        schema: def
                            .get("schema")
                            .map(|s| compile_schema(s, &at))
                            .transpose()?,
                        queryable,
                        allow_commits: def
                            .get("allowCommits")
                            .and_then(|b| b.as_bool())
                            .unwrap_or(false),
                        rules,
                    },
                );
            }
        }

        let mut actions = HashMap::new();
        if let Some(e) = obj.get("events") {
            if e.as_object().is_some_and(|m| !m.is_empty()) {
                return perr("events with projections are gone (ADR-0008); declare actions");
            }
        }
        if let Some(e) = obj.get("actions") {
            let e = e
                .as_object()
                .ok_or_else(|| ProfileError("actions must be an object".into()))?;
            for (name, def) in e {
                let at = format!("actions.{name}");
                if name.starts_with("json.commit") || name.starts_with("nl.vng.openevents.") {
                    return perr(format!("{at}: this event type is reserved"));
                }
                let def = def
                    .as_object()
                    .ok_or_else(|| ProfileError(format!("{at} must be an object")))?;
                let list = def
                    .get("commits")
                    .and_then(|c| c.as_array())
                    .filter(|c| !c.is_empty())
                    .ok_or_else(|| {
                        ProfileError(format!("{at}: commits must be a non-empty list"))
                    })?;
                let mut commits = Vec::new();
                for (i, c) in list.iter().enumerate() {
                    let cat = format!("{at}.commits[{i}]");
                    let c = c
                        .as_object()
                        .ok_or_else(|| ProfileError(format!("{cat} must be an object")))?;
                    let type_name = str_field(c, "type")
                        .ok_or_else(|| ProfileError(format!("{cat}: missing type")))?
                        .to_string();
                    if !types.contains_key(&type_name) {
                        return perr(format!("{cat}: {type_name} is not a type of this profile"));
                    }
                    let op = match str_field(c, "op") {
                        Some("create") => CommitOp::Create,
                        Some("update") => CommitOp::Update,
                        Some("delete") => CommitOp::Delete,
                        other => {
                            return perr(format!(
                                "{cat}: op must be create, update or delete, not {other:?}"
                            ))
                        }
                    };
                    let fields = match c.get("fields") {
                        None => None,
                        Some(f) => Some(
                            f.as_array()
                                .ok_or_else(|| {
                                    ProfileError(format!("{cat}.fields must be a list"))
                                })?
                                .iter()
                                .filter_map(|v| v.as_str().map(String::from))
                                .collect(),
                        ),
                    };
                    let set = match c.get("set") {
                        None => Map::new(),
                        Some(Value::Object(m)) => {
                            check_set(&Value::Object(m.clone()), &format!("{cat}.set"))?;
                            m.clone()
                        }
                        Some(_) => return perr(format!("{cat}.set must be an object")),
                    };
                    let rule = match c.get("rule") {
                        None => None,
                        Some(v) => Some(
                            cql2::parse_any(v)
                                .map_err(|e| ProfileError(format!("{cat}.rule: {e}")))?,
                        ),
                    };
                    commits.push(CommitSpec {
                        type_name,
                        op,
                        fields,
                        set,
                        rule,
                    });
                }
                actions.insert(
                    name.clone(),
                    Action {
                        name: name.clone(),
                        title: str_field(def, "title").map(String::from),
                        commits,
                    },
                );
            }
        }

        Ok(Profile {
            id,
            version,
            raw,
            types,
            actions,
        })
    }
}

/// Error messages of a JSON Schema validation, as (JSON pointer, reason).
pub fn schema_errors(v: &jsonschema::Validator, instance: &Value) -> Vec<(String, String)> {
    v.iter_errors(instance)
        .take(20)
        .map(|e| (e.instance_path.to_string(), e.to_string()))
        .collect()
}

/// `set` holds constants and the placeholders `$actor` and `$time`, nothing else:
/// no expressions, so a profile stays data.
fn check_set(v: &Value, at: &str) -> Result<(), ProfileError> {
    match v {
        Value::String(s) if s.starts_with('$') && s != "$actor" && s != "$time" => perr(format!(
            "{at}: {s} is not allowed; set takes constants, $actor and $time"
        )),
        Value::Object(m) => m
            .iter()
            .try_for_each(|(k, v)| check_set(v, &format!("{at}.{k}"))),
        Value::Array(items) => items.iter().try_for_each(|v| check_set(v, at)),
        _ => Ok(()),
    }
}

/// Fill in `$actor` and `$time` in a `set` value.
pub fn render_set(v: &Value, actor: &str, time: &str) -> Value {
    match v {
        Value::String(s) if s == "$actor" => Value::String(actor.into()),
        Value::String(s) if s == "$time" => Value::String(time.into()),
        Value::Object(m) => Value::Object(
            m.iter()
                .map(|(k, v)| (k.clone(), render_set(v, actor, time)))
                .collect(),
        ),
        Value::Array(items) => {
            Value::Array(items.iter().map(|v| render_set(v, actor, time)).collect())
        }
        other => other.clone(),
    }
}

/// RFC 7396 JSON Merge Patch.
pub fn merge_patch(target: &mut Value, patch: &Value) {
    match patch {
        Value::Object(p) => {
            if !target.is_object() {
                *target = Value::Object(Map::new());
            }
            let t = target.as_object_mut().unwrap();
            for (k, v) in p {
                if v.is_null() {
                    t.remove(k);
                } else {
                    merge_patch(t.entry(k.clone()).or_insert(Value::Null), v);
                }
            }
        }
        other => *target = other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn retention_durations() {
        let r = Retention::parse("eind", "P1Y6M").unwrap();
        assert_eq!(r.months, 18);
        let r = Retention::parse("eind", "P2WT12H").unwrap();
        assert_eq!((r.days, r.seconds), (14, 43200));
        assert!(Retention::parse("eind", "1Y").is_none());
        assert!(Retention::parse("eind", "P").is_none());
        let r = Retention::parse("eind", "P30D").unwrap();
        let e = r.expires(&serde_json::json!({ "eind": "2026-01-01T10:00:00Z" })).unwrap();
        assert_eq!(e.to_rfc3339(), "2026-01-31T10:00:00+00:00");
        assert!(r.expires(&serde_json::json!({ "eind": null })).is_none());
    }

    #[test]
    fn merge_patch_rfc7396() {
        let mut doc = json!({ "a": 1, "b": { "c": 2, "d": 3 }, "l": [1, 2] });
        merge_patch(&mut doc, &json!({ "a": null, "b": { "c": 9 }, "l": [3] }));
        assert_eq!(doc, json!({ "b": { "c": 9, "d": 3 }, "l": [3] }));
    }

    #[test]
    fn set_placeholders() {
        let v = render_set(
            &json!({ "door": "$actor", "x": { "t": "$time" }, "s": "open" }),
            "a",
            "t1",
        );
        assert_eq!(v, json!({ "door": "a", "x": { "t": "t1" }, "s": "open" }));
        assert!(check_set(&json!({ "x": "$data.y" }), "set").is_err());
    }

    #[test]
    fn actions_compile() {
        let p = Profile::compile(json!({
            "id": "t", "types": { "x.gesprek": {}, "x.bijdrage": {} },
            "actions": { "x.bijdrage.toegevoegd": { "commits": [
                { "type": "x.bijdrage", "op": "create", "fields": ["tekst"], "set": { "door": "$actor" },
                  "rule": "gesprek.status = 'open'" } ] } }
        }))
        .unwrap();
        let a = &p.actions["x.bijdrage.toegevoegd"];
        assert_eq!(a.commits[0].op, CommitOp::Create);
        assert!(Profile::compile(
            json!({ "id": "t", "types": {}, "actions": { "json.commit": { "commits": [] } } })
        )
        .is_err());
        assert!(Profile::compile(json!({ "id": "t", "types": { "a": {} }, "actions": { "a.b": { "commits": [ { "type": "b", "op": "create" } ] } } })).is_err());
    }
}

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
    pub allow_commits: bool,
    pub rules: Rules,
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
                types.insert(
                    name.clone(),
                    SubjectType {
                        name: name.clone(),
                        fields,
                        schema: def
                            .get("schema")
                            .map(|s| compile_schema(s, &at))
                            .transpose()?,
                        queryable: def.get("queryable").and_then(|q| q.as_array()).map(|q| {
                            q.iter()
                                .filter_map(|s| s.as_str().map(String::from))
                                .collect()
                        }),
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

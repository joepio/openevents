//! Profiles: a domain on the register, as data (docs/profielen.md).
//!
//! A profile declares subject types (state schema, queryable fields, read and write
//! rules in CQL2) and event types (data schema, which right they need, and a purely
//! declarative projection onto the subject's state).

use std::collections::HashMap;

use serde_json::{Map, Value};

use crate::cql2::{self, Expr};

#[derive(Debug, thiserror::Error)]
#[error("invalid profile: {0}")]
pub struct ProfileError(pub String);

fn perr<T>(msg: impl Into<String>) -> Result<T, ProfileError> {
    Err(ProfileError(msg.into()))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Right {
    Append,
    Write,
}

#[derive(Default)]
pub struct Rules {
    pub read: Option<Expr>,
    pub create: Option<Expr>,
    pub write: Option<Expr>,
    pub append: Option<Expr>,
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

#[derive(Debug, Clone)]
pub enum ProjOp {
    Merge { path: String, value: Value },
    Set { path: String, value: Value },
    Append { path: String, value: Value },
    Remove { path: String },
    Increment { path: String, by: f64 },
}

pub struct EventType {
    pub name: String,
    pub subject_type: String,
    pub creates: bool,
    pub right: Right,
    pub dataschema: Option<jsonschema::Validator>,
    pub projection: Vec<ProjOp>,
}

pub struct Profile {
    pub id: String,
    pub version: String,
    pub raw: Value,
    pub types: HashMap<String, SubjectType>,
    pub events: HashMap<String, EventType>,
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

fn parse_op(v: &Value, at: &str) -> Result<ProjOp, ProfileError> {
    let obj = v
        .as_object()
        .ok_or_else(|| ProfileError(format!("{at}: projection step must be an object")))?;
    let path = str_field(obj, "path").unwrap_or("").to_string();
    let value = obj.get("value").cloned().unwrap_or(Value::Null);
    match str_field(obj, "op") {
        Some("merge") => Ok(ProjOp::Merge { path, value }),
        Some("set") => Ok(ProjOp::Set { path, value }),
        Some("append") => Ok(ProjOp::Append { path, value }),
        Some("remove") => Ok(ProjOp::Remove { path }),
        Some("increment") => Ok(ProjOp::Increment {
            path,
            by: obj.get("by").and_then(|b| b.as_f64()).unwrap_or(1.0),
        }),
        other => perr(format!("{at}: unknown projection op {other:?}")),
    }
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
                            append: rule(r, "append", &at)?,
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
                            .unwrap_or(true),
                        rules,
                    },
                );
            }
        }

        let mut events = HashMap::new();
        if let Some(e) = obj.get("events") {
            let e = e
                .as_object()
                .ok_or_else(|| ProfileError("events must be an object".into()))?;
            for (name, def) in e {
                let at = format!("events.{name}");
                if name.starts_with("json.commit") || name.starts_with("nl.vng.openevents.") {
                    return perr(format!("{at}: this event type is reserved"));
                }
                let def = def
                    .as_object()
                    .ok_or_else(|| ProfileError(format!("{at} must be an object")))?;
                let subject_type = str_field(def, "subjectType")
                    .ok_or_else(|| ProfileError(format!("{at}: missing subjectType")))?
                    .to_string();
                if !types.contains_key(&subject_type) {
                    return perr(format!(
                        "{at}: subjectType {subject_type} is not a type of this profile"
                    ));
                }
                let right = match str_field(def, "right").unwrap_or("write") {
                    "append" => Right::Append,
                    "write" => Right::Write,
                    other => {
                        return perr(format!("{at}: right must be append or write, not {other}"))
                    }
                };
                let projection = match def.get("projection") {
                    None => vec![],
                    Some(p) => p
                        .as_array()
                        .ok_or_else(|| ProfileError(format!("{at}.projection must be a list")))?
                        .iter()
                        .enumerate()
                        .map(|(i, step)| parse_op(step, &format!("{at}.projection[{i}]")))
                        .collect::<Result<_, _>>()?,
                };
                events.insert(
                    name.clone(),
                    EventType {
                        name: name.clone(),
                        subject_type,
                        creates: def
                            .get("creates")
                            .and_then(|b| b.as_bool())
                            .unwrap_or(false),
                        right,
                        dataschema: def
                            .get("dataschema")
                            .map(|s| compile_schema(s, &at))
                            .transpose()?,
                        projection,
                    },
                );
            }
        }

        Ok(Profile {
            id,
            version,
            raw,
            types,
            events,
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

// ---------- projection ----------

/// Values available to templates in a projection step.
pub struct TemplateContext<'a> {
    pub data: &'a Value,
    pub time: &'a str,
    pub actor: &'a str,
    pub id: &'a str,
    pub subject: &'a str,
    pub event_type: &'a str,
}

fn render(v: &Value, ctx: &TemplateContext) -> Value {
    match v {
        Value::String(s) if s.starts_with('$') => match s.as_str() {
            "$data" => ctx.data.clone(),
            "$time" => Value::String(ctx.time.into()),
            "$actor" => Value::String(ctx.actor.into()),
            "$id" => Value::String(ctx.id.into()),
            "$subject" => Value::String(ctx.subject.into()),
            "$type" => Value::String(ctx.event_type.into()),
            other => match other.strip_prefix("$data.") {
                Some(path) => cql2::lookup(ctx.data, path)
                    .first()
                    .map(|v| (*v).clone())
                    .unwrap_or(Value::Null),
                None => v.clone(),
            },
        },
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, v)| (k.clone(), render(v, ctx)))
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.iter().map(|v| render(v, ctx)).collect()),
        other => other.clone(),
    }
}

/// The value at a dotted path, created as empty objects along the way.
fn slot<'a>(doc: &'a mut Value, path: &str) -> &'a mut Value {
    if path.is_empty() {
        return doc;
    }
    let mut cur = doc;
    for part in path.split('.') {
        if !cur.is_object() {
            *cur = Value::Object(Map::new());
        }
        cur = cur
            .as_object_mut()
            .unwrap()
            .entry(part.to_string())
            .or_insert(Value::Null);
    }
    cur
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

/// A path may contain `{$data.x}` parts, filled in from the event (dots in the value
/// become `_`, so the value stays one path segment).
fn render_path(path: &str, ctx: &TemplateContext) -> String {
    let mut out = String::new();
    let mut rest = path;
    while let Some(start) = rest.find('{') {
        out.push_str(&rest[..start]);
        let Some(end) = rest[start..].find('}') else {
            out.push_str(&rest[start..]);
            return out;
        };
        let expr = rest[start + 1..start + end].trim();
        let v = render(&Value::String(expr.to_string()), ctx);
        let text = match v {
            Value::String(s) => s,
            Value::Null => "_".to_string(),
            other => other.to_string(),
        };
        out.push_str(&text.replace('.', "_"));
        rest = &rest[start + end + 1..];
    }
    out.push_str(rest);
    out
}

fn remove_path(state: &mut Value, path: &str) {
    if let Some((parent, last)) = path.rsplit_once('.') {
        if let Some(obj) = slot(state, parent).as_object_mut() {
            obj.remove(last);
        }
    } else if let Some(obj) = state.as_object_mut() {
        obj.remove(path);
    }
}

pub fn apply_projection(state: &mut Value, ops: &[ProjOp], ctx: &TemplateContext) {
    if !state.is_object() {
        *state = Value::Object(Map::new());
    }
    for op in ops {
        let op = match op {
            ProjOp::Merge { path, value } => ProjOp::Merge {
                path: render_path(path, ctx),
                value: value.clone(),
            },
            ProjOp::Set { path, value } => ProjOp::Set {
                path: render_path(path, ctx),
                value: value.clone(),
            },
            ProjOp::Append { path, value } => ProjOp::Append {
                path: render_path(path, ctx),
                value: value.clone(),
            },
            ProjOp::Remove { path } => ProjOp::Remove {
                path: render_path(path, ctx),
            },
            ProjOp::Increment { path, by } => ProjOp::Increment {
                path: render_path(path, ctx),
                by: *by,
            },
        };
        match &op {
            ProjOp::Merge { path, value } => merge_patch(slot(state, path), &render(value, ctx)),
            ProjOp::Set { path, value } => {
                // Setting null removes the field, as in a merge patch.
                let v = render(value, ctx);
                if v.is_null() {
                    remove_path(state, path);
                } else {
                    *slot(state, path) = v;
                }
            }
            ProjOp::Append { path, value } => {
                let v = render(value, ctx);
                if v.is_null() {
                    continue;
                }
                let s = slot(state, path);
                if !s.is_array() {
                    *s = Value::Array(vec![]);
                }
                s.as_array_mut().unwrap().push(v);
            }
            ProjOp::Remove { path } => remove_path(state, path),
            ProjOp::Increment { path, by } => {
                let s = slot(state, path);
                let n = s.as_f64().unwrap_or(0.0) + *by;
                *s = if n.fract() == 0.0 {
                    Value::from(n as i64)
                } else {
                    Value::from(n)
                };
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn projection_steps() {
        let data = json!({ "tekst": "hallo", "door": "x" });
        let ctx = TemplateContext {
            data: &data,
            time: "t1",
            actor: "a",
            id: "e1",
            subject: "s",
            event_type: "x.y",
        };
        let mut state = json!({ "status": "open" });
        apply_projection(
            &mut state,
            &[
                ProjOp::Append {
                    path: "bijdragen".into(),
                    value: json!({ "tekst": "$data.tekst", "op": "$time" }),
                },
                ProjOp::Set {
                    path: "laatsteBijdrage.door".into(),
                    value: json!("$actor"),
                },
                ProjOp::Increment {
                    path: "aantal".into(),
                    by: 1.0,
                },
                ProjOp::Merge {
                    path: "".into(),
                    value: json!({ "status": null, "x": 1 }),
                },
                ProjOp::Append {
                    path: "rollen.{$data.door}".into(),
                    value: json!("$actor"),
                },
            ],
            &ctx,
        );
        assert_eq!(
            state,
            json!({ "bijdragen": [ { "tekst": "hallo", "op": "t1" } ], "laatsteBijdrage": { "door": "a" }, "aantal": 1, "x": 1, "rollen": { "x": ["a"] } })
        );
    }
}

//! An OpenAPI 3.1 document per profile, generated from its schemas, so a supplier can
//! validate events against something its tools already understand.

use serde_json::{json, Map, Value};

fn name(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect()
}

/// The JSON Schema of one commit an action allows: the resource type's fields, limited
/// to the ones the client may set.
fn commit_schema(p: &Value, spec: &Value) -> Value {
    let type_name = spec["type"].as_str().unwrap_or_default();
    let props = &p["types"][type_name]["schema"]["properties"];
    let mut body = json!({ "type": "object" });
    if let Some(fields) = spec["fields"].as_array() {
        let mut allowed = Map::new();
        for f in fields.iter().filter_map(|f| f.as_str()) {
            allowed.insert(f.into(), props.get(f).cloned().unwrap_or(json!({})));
        }
        body["properties"] = Value::Object(allowed);
        body["additionalProperties"] = json!(false);
    } else if props.is_object() {
        body["properties"] = props.clone();
    }
    let rid = json!({ "type": "string", "description": "The resource; defaults to the event's subject for a single commit." });
    let prev = json!({ "type": "string", "description": "Optimistic concurrency: the last commit you saw; 412 when it changed." });
    let mut out = match spec["op"].as_str() {
        Some("create") => json!({ "required": ["resourceData"], "properties": {
            "resourceId": rid, "type": { "const": type_name }, "resourceData": body } }),
        Some("update") => json!({ "required": ["patch"], "properties": {
            "resourceId": rid, "previousCommit": prev, "patch": body } }),
        _ => json!({ "required": ["deleted"], "properties": {
            "resourceId": rid, "previousCommit": prev, "deleted": { "const": true } } }),
    };
    out["type"] = json!("object");
    out["title"] = json!(format!(
        "{} {type_name}",
        spec["op"].as_str().unwrap_or("?")
    ));
    if let Some(set) = spec.get("set") {
        out["description"] = json!(format!("The register sets: {set}"));
    }
    out
}

pub fn for_profile(p: &Value) -> Value {
    let id = p["id"].as_str().unwrap_or_default();
    let mut schemas = Map::new();
    schemas.insert(
        "CloudEvent".into(),
        json!({
            "type": "object",
            "required": ["specversion", "id", "source", "type", "subject"],
            "properties": {
                "specversion": { "const": "1.0" },
                "id": { "type": "string", "minLength": 1, "description": "Unique per source; posting the same source and id again is safe (idempotent)." },
                "source": { "type": "string", "format": "uri-reference" },
                "type": { "type": "string" },
                "subject": { "type": "string", "description": "The resource the event is about." },
                "time": { "type": "string", "format": "date-time" },
                "datacontenttype": { "const": "application/json" },
                "dataschema": { "type": "string", "format": "uri" },
                "data": {}
            }
        }),
    );
    schemas.insert(
        "Problem".into(),
        json!({
            "type": "object",
            "properties": {
                "type": { "type": "string" }, "title": { "type": "string" }, "status": { "type": "integer" },
                "detail": { "type": "string" },
                "invalidParams": { "type": "array", "items": { "type": "object", "properties": {
                    "name": { "type": "string" }, "code": { "type": "string" }, "reason": { "type": "string" } } } }
            }
        }),
    );
    let mut event_refs = Vec::new();
    if let Some(actions) = p["actions"].as_object() {
        let mut names: Vec<_> = actions.keys().collect();
        names.sort();
        for t in names {
            let def = &actions[t];
            let n = format!("Action_{}", name(t));
            let specs: Vec<Value> = def["commits"]
                .as_array()
                .map(|list| list.iter().map(|c| commit_schema(p, c)).collect())
                .unwrap_or_default();
            let mut desc = String::from(def["title"].as_str().unwrap_or(t));
            desc.push_str(". `data` is a commit");
            if specs.len() > 1 {
                desc.push_str(", or several in `data.commits` (all or nothing)");
            }
            desc.push_str(". Fields in `set` are filled in by the register.");
            let commit = json!({ "oneOf": specs });
            schemas.insert(
                n.clone(),
                json!({
                    "description": desc,
                    "allOf": [
                        { "$ref": "#/components/schemas/CloudEvent" },
                        { "type": "object", "required": ["data"], "properties": {
                            "type": { "const": t },
                            "data": { "oneOf": [
                                commit,
                                { "type": "object", "required": ["commits"], "properties": {
                                    "commits": { "type": "array", "minItems": 1, "maxItems": 100, "items": commit }
                                } }
                            ] }
                        } }
                    ]
                }),
            );
            event_refs.push(json!({ "$ref": format!("#/components/schemas/{n}") }));
        }
    }
    let mut type_names = Vec::new();
    let mut state_refs = Vec::new();
    if let Some(types) = p["types"].as_object() {
        let mut names: Vec<_> = types.keys().collect();
        names.sort();
        for t in names {
            let n = format!("State_{}", name(t));
            let mut schema = types[t]
                .get("schema")
                .cloned()
                .unwrap_or(json!({ "type": "object" }));
            if let Some(props) = schema.get_mut("properties").and_then(|v| v.as_object_mut()) {
                props.insert("_meta".into(), json!({ "type": "object", "description": "subject, type, head, sequence, created, updated; limited when field rules apply" }));
            }
            schemas.insert(n.clone(), schema);
            state_refs.push(json!({ "$ref": format!("#/components/schemas/{n}") }));
            type_names.push(json!(t));
        }
    }
    let problem = |d: &str| json!({ "description": d, "content": { "application/problem+json": { "schema": { "$ref": "#/components/schemas/Problem" } } } });
    let event = json!({ "oneOf": event_refs });
    json!({
        "openapi": "3.1.0",
        "info": {
            "title": p["title"].as_str().unwrap_or(id),
            "version": p["version"].as_str().unwrap_or("0"),
            "description": format!("Events and representations of profile `{id}` on OpenEvents. Generated from the profile; the profile is leading.")
        },
        "security": [{ "bearer": [] }],
        "paths": {
            "/events": {
                "post": {
                    "summary": "Store an event, or a batch of events",
                    "requestBody": { "required": true, "content": {
                        "application/cloudevents+json": { "schema": event },
                        "application/json": { "schema": event },
                        "application/cloudevents-batch+json": { "schema": { "type": "array", "maxItems": crate::MAX_BATCH, "items": event } }
                    } },
                    "responses": {
                        "201": { "description": "Stored (or already stored: header Idempotent-Replayed)" },
                        "200": { "description": "Batch: all stored" },
                        "207": { "description": "Batch: outcome per event, some failed" },
                        "401": problem("No valid token"),
                        "403": problem("Not allowed"),
                        "409": problem("Same source and id, different content"),
                        "412": problem("previousCommit does not match"),
                        "422": problem("Rejected, nothing stored; invalidParams says why")
                    }
                }
            },
            "/subjects/{subject}": {
                "get": {
                    "summary": "Current state of a subject",
                    "parameters": [{ "name": "subject", "in": "path", "required": true, "schema": { "type": "string" } }],
                    "responses": {
                        "200": { "description": "State", "content": { "application/json": { "schema": { "oneOf": state_refs } } } },
                        "404": problem("Unknown, or not visible to you")
                    }
                }
            },
            "/query": {
                "post": {
                    "summary": "Subjects of one type that match a CQL2 filter (and your read rule)",
                    "requestBody": { "required": true, "content": { "application/json": { "schema": {
                        "type": "object", "required": ["type"],
                        "properties": {
                            "type": { "enum": type_names },
                            "filter": { "description": "CQL2-text (string) or CQL2-JSON" },
                            "fields": { "type": "array", "items": { "type": "string" } },
                            "pageSize": { "type": "integer", "minimum": 1 },
                            "cursor": { "type": "string" }
                        }
                    } } } },
                    "responses": { "200": { "description": "results, next, feedCursor" }, "422": problem("Invalid filter or field") }
                }
            }
        },
        "components": {
            "securitySchemes": { "bearer": { "type": "http", "scheme": "bearer", "bearerFormat": "JWT" } },
            "schemas": schemas
        }
    })
}

//! An OpenAPI 3.1 document per profile, generated from its schemas, so a supplier can
//! validate events against something its tools already understand.

use serde_json::{json, Map, Value};

fn name(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect()
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
                "previouscommit": { "type": "string", "description": "Optimistic concurrency: the head the client saw; 412 when it changed." },
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
    if let Some(events) = p["events"].as_object() {
        let mut names: Vec<_> = events.keys().collect();
        names.sort();
        for t in names {
            let def = &events[t];
            let n = format!("Event_{}", name(t));
            let mut desc = format!(
                "Subject type: {}.",
                def["subjectType"].as_str().unwrap_or("?")
            );
            if def["creates"].as_bool() == Some(true) {
                desc.push_str(" Creates the subject; the subject must not exist yet.");
            } else {
                desc.push_str(&format!(
                    " Needs the right `{}` on an existing subject.",
                    def["right"].as_str().unwrap_or("write")
                ));
            }
            schemas.insert(
                n.clone(),
                json!({
                    "description": desc,
                    "allOf": [
                        { "$ref": "#/components/schemas/CloudEvent" },
                        { "type": "object", "required": ["data"], "properties": {
                            "type": { "const": t },
                            "data": def.get("dataschema").cloned().unwrap_or(json!({}))
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
                        "412": problem("previouscommit does not match"),
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

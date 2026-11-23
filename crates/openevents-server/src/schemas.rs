//! JSON Schemas from elsewhere, such as the VNG API Lab.
//!
//! A type in a profile may point at a shared schema instead of copying it:
//! `"schema": { "$ref": "https://…/schemas/gesprekken/next.json#/$defs/Gesprek" }`,
//! or extend one with `allOf`. When a profile is registered, OpenEvents fetches what is
//! referenced and stores the profile with those schemas inlined, so the register never
//! needs the network after that and a later change upstream only lands with a new
//! profile version. The type keeps the URLs in `schemaSources`.
//!
//! A local copy comes first: with `OPENEVENTS_SCHEMAS=<dir>`, the schema at
//! `https://host/path` is read from `<dir>/host/path` when that file exists. That pins a
//! version and works offline; `examples/schemas` holds the copies the examples use.

use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

use reqwest::Url;
use serde_json::{json, Value};

/// How deep `$ref`s may nest; deeper means a cycle.
const MAX_DEPTH: usize = 32;

pub struct Schemas {
    dir: Option<PathBuf>,
    client: reqwest::Client,
    cache: Mutex<HashMap<String, Value>>,
}

impl Default for Schemas {
    fn default() -> Self {
        Schemas::new(None)
    }
}

impl Schemas {
    pub fn new(dir: Option<PathBuf>) -> Self {
        Schemas {
            dir,
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(10))
                .build()
                .expect("http client"),
            cache: Mutex::new(HashMap::new()),
        }
    }

    pub fn from_env() -> Self {
        Schemas::new(std::env::var("OPENEVENTS_SCHEMAS").ok().map(PathBuf::from))
    }

    async fn document(&self, url: &str) -> Result<Value, String> {
        if let Some(v) = self.cache.lock().unwrap().get(url) {
            return Ok(v.clone());
        }
        let parsed = Url::parse(url).map_err(|e| format!("{url}: {e}"))?;
        let local = self.dir.as_ref().and_then(|d| {
            let p = d
                .join(parsed.host_str()?)
                .join(parsed.path().trim_start_matches('/'));
            p.is_file().then_some(p)
        });
        let doc: Value = match local {
            Some(p) => serde_json::from_slice(
                &std::fs::read(&p).map_err(|e| format!("{}: {e}", p.display()))?,
            )
            .map_err(|e| format!("{}: {e}", p.display()))?,
            None => {
                if !matches!(parsed.scheme(), "https" | "http") {
                    return Err(format!("{url}: only http(s) schemas can be fetched"));
                }
                self.client
                    .get(url)
                    .send()
                    .await
                    .and_then(|r| r.error_for_status())
                    .map_err(|e| format!("{url}: {e}"))?
                    .json()
                    .await
                    .map_err(|e| format!("{url}: {e}"))?
            }
        };
        self.cache
            .lock()
            .unwrap()
            .insert(url.to_string(), doc.clone());
        Ok(doc)
    }

    /// The profile with every external `$ref` in its type schemas inlined.
    pub async fn resolve_profile(&self, mut raw: Value) -> Result<Value, String> {
        let Some(types) = raw.get_mut("types").and_then(Value::as_object_mut) else {
            return Ok(raw);
        };
        for (name, def) in types.iter_mut() {
            let Some(schema) = def.get("schema").cloned() else {
                continue;
            };
            // Fetch until nothing new is referenced.
            let mut docs: HashMap<String, Value> = HashMap::new();
            let mut todo: BTreeSet<String> = BTreeSet::new();
            refs(&schema, None, &mut todo).map_err(|e| format!("types.{name}: {e}"))?;
            while let Some(url) = todo.pop_first() {
                if docs.contains_key(&url) {
                    continue;
                }
                let doc = self
                    .document(&url)
                    .await
                    .map_err(|e| format!("types.{name}: {e}"))?;
                let mut more = BTreeSet::new();
                refs(&doc, Some(&url), &mut more).map_err(|e| format!("types.{name}: {e}"))?;
                docs.insert(url, doc);
                todo.extend(more.into_iter().filter(|u| !docs.contains_key(u)));
            }
            if docs.is_empty() {
                continue;
            }
            let inlined =
                inline(&schema, None, &docs, 0).map_err(|e| format!("types.{name}: {e}"))?;
            def["schema"] = inlined;
            let mut sources: Vec<&String> = docs.keys().collect();
            sources.sort();
            def["schemaSources"] = json!(sources);
        }
        Ok(raw)
    }
}

/// Where a `$ref` points: a document URL and a JSON Pointer in it. `None` for a
/// reference inside the profile itself.
fn target(r: &str, base: Option<&str>) -> Result<Option<(String, String)>, String> {
    let (doc, frag) = r.split_once('#').unwrap_or((r, ""));
    let url = match (doc.is_empty(), base) {
        (true, None) => return Ok(None),
        (true, Some(b)) => b.to_string(),
        (false, Some(b)) => Url::parse(b)
            .and_then(|b| b.join(doc))
            .map_err(|e| format!("{r}: {e}"))?
            .to_string(),
        (false, None) => Url::parse(doc)
            .map_err(|e| format!("{r}: {e}"))?
            .to_string(),
    };
    Ok(Some((url, frag.to_string())))
}

fn refs(v: &Value, base: Option<&str>, out: &mut BTreeSet<String>) -> Result<(), String> {
    match v {
        Value::Object(m) => {
            if let Some(Value::String(r)) = m.get("$ref") {
                if let Some((url, _)) = target(r, base)? {
                    out.insert(url);
                }
            }
            for x in m.values() {
                refs(x, base, out)?;
            }
        }
        Value::Array(a) => {
            for x in a {
                refs(x, base, out)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn inline(
    v: &Value,
    base: Option<&str>,
    docs: &HashMap<String, Value>,
    depth: usize,
) -> Result<Value, String> {
    if depth > MAX_DEPTH {
        return Err("$ref nests too deep (a cycle?)".into());
    }
    match v {
        Value::Object(m) => {
            if let Some(Value::String(r)) = m.get("$ref") {
                if let Some((url, frag)) = target(r, base)? {
                    let doc = docs.get(&url).ok_or_else(|| format!("{url} not loaded"))?;
                    let found = doc
                        .pointer(&frag)
                        .ok_or_else(|| format!("{r}: {frag} not found in {url}"))?;
                    let mut out = inline(found, Some(&url), docs, depth + 1)?;
                    if let Value::Object(o) = &mut out {
                        o.remove("$id");
                        o.remove("$schema");
                        o.remove("$defs");
                        // Siblings of $ref (a description, an extra keyword) stay.
                        for (k, x) in m {
                            if k != "$ref" {
                                o.insert(k.clone(), inline(x, base, docs, depth + 1)?);
                            }
                        }
                    }
                    return Ok(out);
                }
            }
            let mut out = serde_json::Map::new();
            for (k, x) in m {
                out.insert(k.clone(), inline(x, base, docs, depth + 1)?);
            }
            Ok(Value::Object(out))
        }
        Value::Array(a) => Ok(Value::Array(
            a.iter()
                .map(|x| inline(x, base, docs, depth + 1))
                .collect::<Result<_, _>>()?,
        )),
        other => Ok(other.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn inlines_a_lab_schema_from_a_local_copy() {
        let dir = PathBuf::from(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../examples/schemas"
        ));
        let s = Schemas::new(Some(dir));
        let url = "https://raw.githubusercontent.com/VNG-Realisatie/vng-api-lab/main/schemas/gesprekken/next.json";
        let raw = json!({ "id": "x", "types": { "t": { "schema": {
            "allOf": [ { "$ref": format!("{url}#/$defs/Gesprek") }, { "properties": { "team": { "type": "string" } } } ]
        } } } });
        let out = s.resolve_profile(raw).await.unwrap();
        let g = &out["types"]["t"]["schema"]["allOf"][0];
        assert_eq!(g["required"][0], "gespreksonderwerp");
        // The deelname inside the gesprek is inlined too, from the same document.
        assert_eq!(
            g["properties"]["gespreksdeelnames"]["items"]["required"],
            json!(["type", "naam"])
        );
        assert_eq!(out["types"]["t"]["schemaSources"], json!([url]));
    }
}

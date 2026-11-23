//! Adapter for ZGW Notificaties (Open Notificaties, NRC 1.0): register `POST /notificaties`
//! as the callback of an abonnement, and every notificatie becomes a CloudEvent.
//!
//! - type `nl.vng.zgw.<kanaal>.<resource>.<actie>`, e.g. `nl.vng.zgw.zaken.status.create`.
//!   Only types that a profile registers are stored; the rest is acknowledged and ignored,
//!   so a profile decides what to mirror.
//! - subject is the `hoofdObject` (the zaak URL). data holds the change, not the whole
//!   resource: `patch` is a JSON Merge Patch (RFC 7396). For a new resource that is the
//!   whole resource; for an update of the hoofdObject only the fields that differ from
//!   the current state. An update without changes stores no event. Plus the
//!   notificatie's `resourceUrl` and `kenmerken`. The resource is fetched with a ZGW JWT
//!   (client id and secret), except on `destroy`.
//!   The delta is computed against what OpenEvents last saw, because a ZGW notificatie
//!   says that something changed, not what. A source that publishes its own changes
//!   (JSON Commit) makes this step unnecessary.
//! - The event id is derived from the notificatie, so a notificatie that Open Notificaties
//!   delivers twice is stored once.
//! - Order: when a notificatie arrives for a zaak that OpenEvents does not know yet (it
//!   was created before the abonnement, or notificaties arrive out of order), the adapter
//!   first fetches the hoofdObject and stores it as `<kanaal>.<hoofdresource>.create`.
//!
//! Configuration (environment):
//! - `OPENEVENTS_NOTIFICATIES_AUTH`: the exact Authorization header value Open
//!   Notificaties sends (the `auth` of the abonnement). Without it the endpoint is off.
//! - `OPENEVENTS_ZGW_CLIENT_ID`, `OPENEVENTS_ZGW_SECRET`: to fetch resources.
//! - `OPENEVENTS_NOTIFICATIES_ACTOR`: actor id for the events (default
//!   `urn:openevents:notificaties`), with claim `rol=systeem`.
//! - `OPENEVENTS_NOTIFICATIES_API`, `OPENEVENTS_NOTIFICATIES_KANALEN` (comma separated)
//!   and `OPENEVENTS_PUBLIC_URL`: OpenEvents subscribes itself at Open Notificaties and
//!   checks every few minutes that the abonnement is still there (it survives a reset of
//!   Open Notificaties). So nothing has to be configured in Open Notificaties.
//! - `OPENEVENTS_IMPORT_ZAKEN`: root of a Zaken API (`https://…/zaken/api/v1`). At startup
//!   OpenEvents walks all zaken there, with their rollen, statussen, resultaat and
//!   documenten, as if a notificatie had come for each. So zaken from before the
//!   abonnement are mirrored too. Running it again changes nothing.

use std::time::Duration;

use openevents_core::{Actor, Register, RegisterError};
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Clone)]
pub struct NotificatiesConfig {
    pub auth: String,
    pub zgw_client_id: Option<String>,
    pub zgw_secret: Option<String>,
    pub actor: String,
    pub client: reqwest::Client,
    /// Zaken API root for the one-time import at startup.
    pub import_zaken: Option<String>,
    /// Self-subscription: Notificaties API root, kanalen, and our callback URL.
    pub subscribe: Option<Subscribe>,
}

#[derive(Clone)]
pub struct Subscribe {
    pub api: String,
    pub kanalen: Vec<String>,
    pub callback: String,
}

impl NotificatiesConfig {
    pub fn from_env() -> Option<Self> {
        let env = |k: &str| std::env::var(k).ok().filter(|s| !s.is_empty());
        Some(NotificatiesConfig {
            auth: env("OPENEVENTS_NOTIFICATIES_AUTH")?,
            zgw_client_id: env("OPENEVENTS_ZGW_CLIENT_ID"),
            zgw_secret: env("OPENEVENTS_ZGW_SECRET"),
            actor: env("OPENEVENTS_NOTIFICATIES_ACTOR")
                .unwrap_or_else(|| "urn:openevents:notificaties".into()),
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(15))
                .build()
                .expect("http client"),
            import_zaken: env("OPENEVENTS_IMPORT_ZAKEN")
                .map(|u| u.trim_end_matches('/').to_string()),
            subscribe: match (
                env("OPENEVENTS_NOTIFICATIES_API"),
                env("OPENEVENTS_PUBLIC_URL"),
            ) {
                (Some(api), Some(public)) => Some(Subscribe {
                    api: api.trim_end_matches('/').to_string(),
                    kanalen: env("OPENEVENTS_NOTIFICATIES_KANALEN")
                        .unwrap_or_else(|| "zaken".into())
                        .split(',')
                        .map(|k| k.trim().to_string())
                        .filter(|k| !k.is_empty())
                        .collect(),
                    callback: format!("{}/notificaties", public.trim_end_matches('/')),
                }),
                _ => None,
            },
        })
    }

    /// Make sure our abonnement exists. Returns true when it had to be created.
    pub async fn ensure_subscribed(&self) -> Result<bool, String> {
        let Some(sub) = &self.subscribe else {
            return Ok(false);
        };
        let token = self
            .zgw_token()
            .ok_or("no OPENEVENTS_ZGW_CLIENT_ID/SECRET")?;
        let list: Value = self
            .client
            .get(format!("{}/abonnement", sub.api))
            .bearer_auth(&token)
            .send()
            .await
            .and_then(|r| r.error_for_status())
            .map_err(|e| format!("abonnementen: {e}"))?
            .json()
            .await
            .map_err(|e| format!("abonnementen: {e}"))?;
        // Plain list, or paginated ({results: [...]}) depending on the version.
        let items = list
            .as_array()
            .or_else(|| list["results"].as_array())
            .cloned()
            .unwrap_or_default();
        if items
            .iter()
            .any(|a| a["callbackUrl"].as_str() == Some(sub.callback.as_str()))
        {
            return Ok(false);
        }
        let body = json!({
            "callbackUrl": sub.callback,
            "auth": self.auth,
            "kanalen": sub.kanalen.iter().map(|k| json!({ "naam": k, "filters": {} })).collect::<Vec<_>>(),
        });
        let res = self
            .client
            .post(format!("{}/abonnement", sub.api))
            .bearer_auth(&token)
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("abonnement: {e}"))?;
        if !res.status().is_success() {
            let status = res.status();
            let text = res.text().await.unwrap_or_default();
            return Err(format!(
                "abonnement: {status} {}",
                text.chars().take(300).collect::<String>()
            ));
        }
        Ok(true)
    }

    /// Check the abonnement now and then, for as long as the server runs.
    pub async fn keep_subscribed(self: std::sync::Arc<Self>) {
        loop {
            match self.ensure_subscribed().await {
                Ok(true) => tracing::info!("subscribed at Open Notificaties"),
                Ok(false) => {}
                Err(e) => tracing::warn!("Open Notificaties: {e}"),
            }
            tokio::time::sleep(Duration::from_secs(300)).await;
        }
    }

    /// Does the Authorization header match? Compared in constant time.
    pub fn authorized(&self, header: Option<&str>) -> bool {
        let Some(h) = header else { return false };
        let (a, b) = (h.as_bytes(), self.auth.as_bytes());
        a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
    }

    fn actor(&self) -> Actor {
        Actor {
            id: self.actor.clone(),
            claims: json!({ "sub": self.actor, "rol": "systeem" }),
            admin: false,
        }
    }

    /// The ZGW way to authenticate: a JWT signed with the shared secret.
    fn zgw_token(&self) -> Option<String> {
        let (id, secret) = (self.zgw_client_id.as_ref()?, self.zgw_secret.as_ref()?);
        let now = chrono::Utc::now().timestamp();
        let claims = json!({
            "iss": id, "client_id": id, "iat": now,
            "user_id": self.actor, "user_representation": "OpenEvents"
        });
        jsonwebtoken::encode(
            &jsonwebtoken::Header::new(jsonwebtoken::Algorithm::HS256),
            &claims,
            &jsonwebtoken::EncodingKey::from_secret(secret.as_bytes()),
        )
        .ok()
    }

    /// Names that only live in the catalogus, copied on the resource: the
    /// `statustypeOmschrijving` of a status ("Ontvangen", "In behandeling").
    async fn enrich(&self, resource: &str, r: &mut Value) {
        if resource == "status" {
            if let Some(url) = r["statustype"].as_str().map(String::from) {
                if let Ok(st) = self.fetch(&url).await {
                    if let Some(o) = st["omschrijving"].as_str() {
                        r["statustypeOmschrijving"] = json!(o);
                    }
                }
            }
        }
    }

    async fn fetch(&self, url: &str) -> Result<Value, String> {
        let mut req = self.client.get(url).header("Accept-Crs", "EPSG:4326");
        if let Some(t) = self.zgw_token() {
            req = req.bearer_auth(t);
        }
        req.send()
            .await
            .and_then(|r| r.error_for_status())
            .map_err(|e| format!("{url}: {e}"))?
            .json()
            .await
            .map_err(|e| format!("{url}: {e}"))
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Notificatie {
    pub kanaal: String,
    pub hoofd_object: String,
    pub resource: String,
    pub resource_url: String,
    pub actie: String,
    pub aanmaakdatum: Option<String>,
    #[serde(default)]
    pub kenmerken: Value,
}

/// JSON Merge Patch from `old` to `new`, over the fields of `new` only (the mirror may
/// hold fields the adapter adds, such as `initiatorBsn`). `None`: no change.
pub fn merge_patch(old: &Value, new: &Value) -> Option<Value> {
    match (old, new) {
        (Value::Object(o), Value::Object(n)) => {
            let mut out = serde_json::Map::new();
            for (k, nv) in n {
                match o.get(k) {
                    Some(ov) if ov == nv => {}
                    Some(ov @ Value::Object(_)) if nv.is_object() => {
                        if let Some(p) = merge_patch(ov, nv) {
                            out.insert(k.clone(), p);
                        }
                    }
                    // A null in a merge patch means "remove"; a field that was already
                    // absent and is null now did not change.
                    None if nv.is_null() => {}
                    _ => {
                        out.insert(k.clone(), nv.clone());
                    }
                }
            }
            (!out.is_empty()).then_some(Value::Object(out))
        }
        _ if old == new => None,
        _ => Some(new.clone()),
    }
}

pub enum Outcome {
    Stored,
    Ignored(String),
}

/// The resource name of the hoofdObject per kanaal, for fetching it when it is missing.
fn hoofdresource(kanaal: &str) -> Option<&'static str> {
    match kanaal {
        "zaken" => Some("zaak"),
        "besluiten" => Some("besluit"),
        "documenten" => Some("enkelvoudiginformatieobject"),
        _ => None,
    }
}

fn event_id(parts: &[&str]) -> String {
    uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_URL, parts.join("|").as_bytes()).to_string()
}

/// The mirrored state of a resource, or `None` when it is not (or no longer) there.
fn mirrored(register: &Register, actor: &Actor, url: &str) -> Option<Value> {
    let mut v = register.get_subject(actor, url).ok()?;
    v.as_object_mut()?.remove("_meta");
    Some(v)
}

/// A notificatie becomes one action whose commits change the mirror: the resource
/// itself (created, patched with what changed, or deleted) and, for a subresource, the
/// zaak it belongs to (ADR-0008). Event type `nl.vng.zgw.<kanaal>.<resource>.<actie>`,
/// resource type `nl.vng.zgw.<resource>`.
pub async fn handle(
    cfg: &NotificatiesConfig,
    register: &Register,
    n: Notificatie,
) -> Result<Outcome, String> {
    let mut actie = n.actie.clone();
    let kind = |actie: &str| format!("nl.vng.zgw.{}.{}.{actie}", n.kanaal, n.resource);
    if !register.knows_action(&kind(&actie)) {
        return Ok(Outcome::Ignored(format!("no profile for {}", kind(&actie))));
    }
    let reader = Actor {
        id: cfg.actor.clone(),
        claims: json!({}),
        admin: true,
    };
    let actor = cfg.actor();
    let time = n
        .aanmaakdatum
        .clone()
        .unwrap_or_else(|| chrono::Utc::now().to_rfc3339());
    let source = format!("urn:zgw:notificaties:{}", n.kanaal);
    let is_hoofd = n.resource_url == n.hoofd_object;

    // A subresource of a zaak we have not seen: mirror the zaak first.
    if !is_hoofd && mirrored(register, &reader, &n.hoofd_object).is_none() {
        let hoofd = hoofdresource(&n.kanaal).ok_or("unknown kanaal for the hoofdObject")?;
        let create = format!("nl.vng.zgw.{}.{hoofd}.create", n.kanaal);
        if !register.knows_action(&create) {
            return Err(format!(
                "{} is unknown and there is no {create}",
                n.hoofd_object
            ));
        }
        let zaak = cfg.fetch(&n.hoofd_object).await?;
        let mirror = json!({
            "specversion": "1.0",
            "id": event_id(&[&n.hoofd_object, "spiegel"]),
            "source": source,
            "type": create,
            "subject": n.hoofd_object,
            "time": time,
            "gespiegeld": true,
            "data": { "type": format!("nl.vng.zgw.{hoofd}"), "resourceData": zaak },
        });
        match register.submit(&actor, mirror).await {
            Ok(_) | Err(RegisterError::Conflict(_)) => {}
            Err(e) => return Err(format!("mirror {}: {e:?}", n.hoofd_object)),
        }
    }

    let current = mirrored(register, &reader, &n.resource_url);
    let mut commits = Vec::new();
    let mut new_resource = Value::Null;
    match (actie.as_str(), &current) {
        ("destroy", None) => return Ok(Outcome::Ignored("already gone".into())),
        ("destroy", Some(_)) => {
            commits.push(json!({ "resourceId": n.resource_url, "deleted": true }));
        }
        ("create", Some(_)) => {
            return Ok(Outcome::Ignored(format!(
                "{} was already mirrored",
                n.resource_url
            )))
        }
        (_, None) => {
            // A create, or a change to something we missed: mirror it whole.
            actie = "create".into();
            new_resource = cfg.fetch(&n.resource_url).await?;
            cfg.enrich(&n.resource, &mut new_resource).await;
            commits.push(json!({
                "resourceId": n.resource_url,
                "type": format!("nl.vng.zgw.{}", n.resource),
                "resourceData": new_resource,
            }));
        }
        (_, Some(old)) => {
            new_resource = cfg.fetch(&n.resource_url).await?;
            cfg.enrich(&n.resource, &mut new_resource).await;
            if let Some(patch) = merge_patch(old, &new_resource) {
                commits.push(json!({ "resourceId": n.resource_url, "patch": patch }));
            }
        }
    }

    // A subresource also changes the zaak (Open Zaak sets `status`, `resultaat`, ...).
    // An initiator's BSN is copied onto the zaak, so the zaak's read rule can use it.
    if !is_hoofd {
        if let Some(old_zaak) = mirrored(register, &reader, &n.hoofd_object) {
            let mut zaak = cfg.fetch(&n.hoofd_object).await?;
            let bsn = new_resource["betrokkeneIdentificatie"]["inpBsn"].as_str();
            if n.resource == "rol"
                && new_resource["omschrijvingGeneriek"] == "initiator"
                && bsn.is_some()
            {
                let mut list: Vec<Value> = old_zaak["initiatorBsn"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default();
                if !list.iter().any(|b| b.as_str() == bsn) {
                    list.push(json!(bsn));
                }
                zaak["initiatorBsn"] = Value::Array(list);
            }
            // Keep what the adapter added (initiatorBsn above), and give the zaak the
            // name of its current status, so a portal does not have to look it up.
            for k in ["initiatorBsn", "statusOmschrijving"] {
                if zaak.get(k).is_none() && !old_zaak[k].is_null() {
                    zaak[k] = old_zaak[k].clone();
                }
            }
            if n.resource == "status" && zaak["status"] == n.resource_url {
                if let Some(o) = new_resource["statustypeOmschrijving"].as_str() {
                    zaak["statusOmschrijving"] = json!(o);
                }
            }
            if let Some(patch) = merge_patch(&old_zaak, &zaak) {
                commits.push(json!({ "resourceId": n.hoofd_object, "patch": patch }));
            }
        }
    }
    if commits.is_empty() {
        return Ok(Outcome::Ignored("no change".into()));
    }
    let data = if commits.len() == 1 {
        commits.pop().unwrap()
    } else {
        json!({ "commits": commits })
    };
    let event = json!({
        "specversion": "1.0",
        "id": event_id(&[&n.resource_url, &n.actie, &time]),
        "source": source,
        "type": kind(&actie),
        "subject": n.resource_url,
        "time": time,
        "datacontenttype": "application/json",
        "data": data,
    });
    match register.submit(&actor, event).await {
        Ok(_) | Err(RegisterError::Conflict(_)) => Ok(Outcome::Stored),
        Err(e) => Err(format!("{e:?}")),
    }
}

/// All results of a ZGW list: paginated (`results`, `next`) or a plain array.
async fn list_all(cfg: &NotificatiesConfig, url: &str) -> Result<Vec<Value>, String> {
    let mut out = Vec::new();
    let mut next = Some(url.to_string());
    let mut pages = 0;
    while let Some(u) = next.take() {
        let page = cfg.fetch(&u).await?;
        match page {
            Value::Array(a) => out.extend(a),
            p => {
                out.extend(p["results"].as_array().cloned().unwrap_or_default());
                next = p["next"].as_str().map(String::from);
            }
        }
        pages += 1;
        if pages >= 200 {
            break;
        }
    }
    Ok(out)
}

/// The import at startup (`OPENEVENTS_IMPORT_ZAKEN`): every zaak and what belongs to it
/// goes through `handle`, as a `create` notificatie. What is already mirrored is skipped.
pub async fn import_zaken(
    cfg: &NotificatiesConfig,
    register: &Register,
    api: &str,
) -> Result<(usize, usize), String> {
    let (mut stored, mut seen) = (0, 0);
    let mut one = |r: Result<Outcome, String>, what: &str| match r {
        Ok(Outcome::Stored) => stored += 1,
        Ok(Outcome::Ignored(_)) => {}
        Err(e) => tracing::warn!("import {what}: {e}"),
    };
    for zaak in list_all(cfg, &format!("{api}/zaken")).await? {
        let Some(url) = zaak["url"].as_str().map(String::from) else {
            continue;
        };
        seen += 1;
        let n = |resource: &str, resource_url: &str, time: Option<&str>| Notificatie {
            kanaal: "zaken".into(),
            hoofd_object: url.clone(),
            resource: resource.into(),
            resource_url: resource_url.into(),
            actie: "create".into(),
            aanmaakdatum: time.map(String::from),
            kenmerken: Value::Null,
        };
        // Events carry the original moment, so a history reads in the right order.
        let day = |d: Option<&str>| {
            d.map(|d| {
                if d.len() == 10 {
                    format!("{d}T00:00:00Z")
                } else {
                    d.to_string()
                }
            })
        };
        let zaak_time = day(zaak["registratiedatum"]
            .as_str()
            .or(zaak["startdatum"].as_str()));
        one(
            handle(cfg, register, n("zaak", &url, zaak_time.as_deref())).await,
            &url,
        );
        let q = format!("zaak={}", urlencoding(&url));
        // Oldest status first, so the zaak ends with its current status.
        let mut statussen = list_all(cfg, &format!("{api}/statussen?{q}"))
            .await
            .unwrap_or_default();
        statussen.sort_by(|a, b| {
            a["datumStatusGezet"]
                .as_str()
                .cmp(&b["datumStatusGezet"].as_str())
        });
        for (resource, list) in [
            (
                "rol",
                list_all(cfg, &format!("{api}/rollen?{q}"))
                    .await
                    .unwrap_or_default(),
            ),
            ("status", statussen),
            (
                "resultaat",
                list_all(cfg, &format!("{api}/resultaten?{q}"))
                    .await
                    .unwrap_or_default(),
            ),
            (
                "zaakinformatieobject",
                list_all(cfg, &format!("{api}/zaakinformatieobjecten?{q}"))
                    .await
                    .unwrap_or_default(),
            ),
        ] {
            for r in list {
                if let Some(u) = r["url"].as_str() {
                    let t = day(r["datumStatusGezet"]
                        .as_str()
                        .or(r["registratiedatum"].as_str()))
                    .or(zaak_time.clone());
                    one(handle(cfg, register, n(resource, u, t.as_deref())).await, u);
                }
            }
        }
    }
    Ok((seen, stored))
}

fn urlencoding(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

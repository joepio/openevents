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

/// JSON Merge Patch from `old` to `new`, over the fields of `new` only (the state also
/// holds fields that the projection adds, such as `statussen`). `None`: no change.
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

fn subject_exists(e: &RegisterError) -> bool {
    matches!(e, RegisterError::Invalid(_, params)
        if params.iter().any(|p| p.code == "exists" && p.name == "/subject"))
}

fn subject_missing(e: &RegisterError) -> bool {
    matches!(e, RegisterError::Invalid(_, params)
        if params.iter().any(|p| p.code == "not-found" && p.name == "/subject"))
}

pub async fn handle(
    cfg: &NotificatiesConfig,
    register: &Register,
    n: Notificatie,
) -> Result<Outcome, String> {
    let kind = format!("nl.vng.zgw.{}.{}.{}", n.kanaal, n.resource, n.actie);
    if !register.knows_event_type(&kind) {
        return Ok(Outcome::Ignored(format!("no profile for {kind}")));
    }
    let resource = if n.actie == "destroy" {
        Value::Null
    } else {
        cfg.fetch(&n.resource_url).await?
    };
    // The change: against the current state when the hoofdObject itself changed,
    // otherwise the whole (new) subresource.
    let patch = if n.actie == "destroy" {
        Value::Null
    } else if n.resource_url == n.hoofd_object && n.actie != "create" {
        let reader = Actor {
            id: cfg.actor.clone(),
            claims: json!({}),
            admin: true,
        };
        match register.get_subject(&reader, &n.hoofd_object) {
            Ok(mut current) => {
                if let Some(o) = current.as_object_mut() {
                    o.remove("_meta");
                }
                match merge_patch(&current, &resource) {
                    Some(p) => p,
                    None => return Ok(Outcome::Ignored("no change".into())),
                }
            }
            Err(_) => resource.clone(),
        }
    } else {
        resource.clone()
    };
    let time = n
        .aanmaakdatum
        .clone()
        .unwrap_or_else(|| chrono::Utc::now().to_rfc3339());
    let source = format!("urn:zgw:notificaties:{}", n.kanaal);
    let event = json!({
        "specversion": "1.0",
        "id": event_id(&[&n.resource_url, &n.actie, &time]),
        "source": source,
        "type": kind,
        "subject": n.hoofd_object,
        "time": time,
        "datacontenttype": "application/json",
        "data": { "patch": patch, "resourceUrl": n.resource_url, "kenmerken": n.kenmerken },
    });
    let actor = cfg.actor();
    match register.submit(&actor, event.clone()).await {
        Ok(_) => Ok(Outcome::Stored),
        Err(e) if subject_missing(&e) => {
            // Unknown zaak: mirror it first, then store this notificatie.
            let hoofd = hoofdresource(&n.kanaal).ok_or("unknown kanaal for the hoofdObject")?;
            let create = format!("nl.vng.zgw.{}.{hoofd}.create", n.kanaal);
            if !register.knows_event_type(&create) {
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
                "datacontenttype": "application/json",
                "data": { "patch": zaak, "resourceUrl": n.hoofd_object, "kenmerken": {}, "gespiegeld": true },
            });
            register
                .submit(&actor, mirror)
                .await
                .map_err(|e| format!("mirror {}: {e:?}", n.hoofd_object))?;
            register
                .submit(&actor, event)
                .await
                .map(|_| Outcome::Stored)
                .map_err(|e| format!("{e:?}"))
        }
        Err(RegisterError::Conflict(_)) => Ok(Outcome::Stored),
        // A create for a zaak that was already mirrored: the mirror has the same data.
        Err(e) if subject_exists(&e) => Ok(Outcome::Ignored(format!(
            "{} was already mirrored",
            n.hoofd_object
        ))),
        Err(e) => Err(format!("{e:?}")),
    }
}

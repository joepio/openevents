//! Adapter for ZGW Notificaties (Open Notificaties, NRC 1.0): register `POST /notificaties`
//! as the callback of an abonnement, and every notificatie becomes a CloudEvent.
//!
//! - type `nl.vng.zgw.<kanaal>.<resource>.<actie>`, e.g. `nl.vng.zgw.zaken.status.create`.
//!   Only types that a profile registers are stored; the rest is acknowledged and ignored,
//!   so a profile decides what to mirror.
//! - subject is the `hoofdObject` (the zaak URL), data is the fetched resource plus the
//!   notificatie's `resourceUrl` and `kenmerken`. The resource is fetched with a ZGW
//!   JWT (client id and secret), except on `destroy`.
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
        })
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
        "data": { "resource": resource, "resourceUrl": n.resource_url, "kenmerken": n.kenmerken },
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
                "data": { "resource": zaak, "resourceUrl": n.hoofd_object, "kenmerken": {}, "gespiegeld": true },
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

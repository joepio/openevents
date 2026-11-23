//! Who is calling. Tokens come from outside (requirement A7); the register has no login.
//!
//! - `Authorization: Bearer <JWT>` from an OpenID Connect provider (`OPENEVENTS_OIDC_ISSUER`,
//!   keys from its JWKS, refreshed when an unknown `kid` shows up). This is how suppliers
//!   connect: OAuth2 client credentials at their or the municipality's IdP.
//! - `Authorization: Bearer <JWT>`, HS256 with `OPENEVENTS_JWT_SECRET` (simple setups).
//! - The actor id is the claim `OPENEVENTS_ACTOR_CLAIM` (default `sub`; for client
//!   credentials often `azp` or `client_id`), after `OPENEVENTS_ACTOR_PREFIX` when set
//!   (`https://id.example.nl/` + `suzanne`), so login ids match the ids in the data. All claims are available to `actor('claim')`
//!   in rules, also nested ones such as `actor('realm_access.roles')`.
//! - With `OPENEVENTS_DEV_AUTH=true` also `Bearer dev:<actor-id>|claim=value|...`, for
//!   demos and tests only.
//! - Actors listed in `OPENEVENTS_ADMINS` (comma separated) are administrators.
//! - For `EventSource` in browsers, which cannot send headers: `POST /events/tickets`
//!   returns a short-lived ticket for `GET /events?ticket=...`, so tokens stay out of URLs.

use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use axum::http::HeaderMap;
use dashmap::DashMap;
use jsonwebtoken::jwk::JwkSet;
use jsonwebtoken::{Algorithm, DecodingKey, Validation};
use openevents_core::Actor;
use serde_json::{Map, Value};

#[derive(Clone, Default)]
pub struct AuthConfig {
    pub jwt_secret: Option<String>,
    pub dev_auth: bool,
    pub admins: Vec<String>,
    pub oidc: Option<Arc<Oidc>>,
    /// Claim that holds the actor id. Empty means `sub`.
    pub actor_claim: String,
    /// Put before the claim value to make the actor id.
    pub actor_prefix: String,
}

/// Tokens from an OpenID Connect provider, checked against its JWKS.
pub struct Oidc {
    /// Expected `iss`. Also used for discovery when `jwks_url` is empty.
    pub issuer: Option<String>,
    pub jwks_url: RwLock<Option<String>>,
    /// Expected `aud`, when set.
    pub audience: Option<String>,
    keys: RwLock<(JwkSet, Option<Instant>)>,
    client: reqwest::Client,
}

/// Unknown `kid`s trigger a refresh, at most this often.
const JWKS_MIN_REFRESH: Duration = Duration::from_secs(30);

impl Oidc {
    pub fn new(issuer: Option<String>, jwks_url: Option<String>, audience: Option<String>) -> Self {
        Oidc {
            issuer: issuer.map(|i| i.trim_end_matches('/').to_string()),
            jwks_url: RwLock::new(jwks_url),
            audience,
            keys: RwLock::new((JwkSet { keys: vec![] }, None)),
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(10))
                .build()
                .expect("http client"),
        }
    }

    /// Fetch the keys (and first the discovery document, if needed).
    pub async fn refresh(&self) -> Result<usize, String> {
        let url = self.jwks_url.read().unwrap().clone();
        let url = match url {
            Some(u) => u,
            None => {
                let issuer = self.issuer.as_ref().ok_or("no issuer and no JWKS URL")?;
                let doc: Value = self
                    .client
                    .get(format!("{issuer}/.well-known/openid-configuration"))
                    .send()
                    .await
                    .and_then(|r| r.error_for_status())
                    .map_err(|e| format!("discovery: {e}"))?
                    .json()
                    .await
                    .map_err(|e| format!("discovery: {e}"))?;
                let u = doc["jwks_uri"]
                    .as_str()
                    .ok_or("discovery document has no jwks_uri")?
                    .to_string();
                *self.jwks_url.write().unwrap() = Some(u.clone());
                u
            }
        };
        let set: JwkSet = self
            .client
            .get(&url)
            .send()
            .await
            .and_then(|r| r.error_for_status())
            .map_err(|e| format!("jwks: {e}"))?
            .json()
            .await
            .map_err(|e| format!("jwks: {e}"))?;
        let n = set.keys.len();
        *self.keys.write().unwrap() = (set, Some(Instant::now()));
        Ok(n)
    }

    fn may_refresh(&self) -> bool {
        match self.keys.read().unwrap().1 {
            Some(t) => t.elapsed() >= JWKS_MIN_REFRESH,
            None => true,
        }
    }

    /// `None` when the key is unknown (worth a refresh), `Some(None)` when invalid.
    fn verify(&self, token: &str, header: &jsonwebtoken::Header) -> Option<Option<Value>> {
        let keys = self.keys.read().unwrap();
        let jwk = match &header.kid {
            Some(kid) => keys.0.find(kid)?,
            None if keys.0.keys.len() == 1 => &keys.0.keys[0],
            None => return Some(None),
        };
        let Ok(key) = DecodingKey::from_jwk(jwk) else {
            return Some(None);
        };
        if matches!(
            header.alg,
            Algorithm::HS256 | Algorithm::HS384 | Algorithm::HS512
        ) {
            return Some(None);
        }
        let mut v = Validation::new(header.alg);
        match &self.audience {
            Some(a) => v.set_audience(&[a]),
            None => v.validate_aud = false,
        }
        if let Some(i) = &self.issuer {
            v.set_issuer(&[i, &format!("{i}/")]);
        }
        Some(
            jsonwebtoken::decode::<Value>(token, &key, &v)
                .ok()
                .map(|d| d.claims),
        )
    }
}

impl AuthConfig {
    pub fn from_env() -> Self {
        let env = |k: &str| std::env::var(k).ok().filter(|s| !s.is_empty());
        let issuer = env("OPENEVENTS_OIDC_ISSUER");
        let jwks = env("OPENEVENTS_OIDC_JWKS_URL");
        let oidc = (issuer.is_some() || jwks.is_some())
            .then(|| Arc::new(Oidc::new(issuer, jwks, env("OPENEVENTS_OIDC_AUDIENCE"))));
        AuthConfig {
            oidc,
            actor_claim: env("OPENEVENTS_ACTOR_CLAIM").unwrap_or_default(),
            actor_prefix: env("OPENEVENTS_ACTOR_PREFIX").unwrap_or_default(),
            jwt_secret: std::env::var("OPENEVENTS_JWT_SECRET")
                .ok()
                .filter(|s| !s.is_empty()),
            dev_auth: std::env::var("OPENEVENTS_DEV_AUTH")
                .map(|v| v == "true" || v == "1")
                .unwrap_or(false),
            admins: std::env::var("OPENEVENTS_ADMINS")
                .map(|v| {
                    v.split(',')
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect()
                })
                .unwrap_or_default(),
        }
    }

    fn actor_from_claims(&self, claims: Value) -> Option<Actor> {
        let claim = if self.actor_claim.is_empty() {
            "sub"
        } else {
            &self.actor_claim
        };
        let id = format!("{}{}", self.actor_prefix, claims.get(claim)?.as_str()?);
        Some(self.actor(id, claims))
    }

    fn actor(&self, id: String, claims: Value) -> Actor {
        let admin = self.admins.contains(&id);
        Actor { id, claims, admin }
    }

    pub fn from_token(&self, token: &str) -> Option<Actor> {
        if self.dev_auth {
            if let Some(rest) = token.strip_prefix("dev:") {
                let mut parts = rest.split('|');
                let id = parts.next()?.to_string();
                if id.is_empty() {
                    return None;
                }
                let mut claims = Map::new();
                claims.insert("sub".into(), Value::String(id.clone()));
                for p in parts {
                    if let Some((k, v)) = p.split_once('=') {
                        let vals: Vec<Value> =
                            v.split(',').map(|s| Value::String(s.to_string())).collect();
                        claims.insert(
                            k.to_string(),
                            if vals.len() == 1 {
                                vals[0].clone()
                            } else {
                                Value::Array(vals)
                            },
                        );
                    }
                }
                return Some(self.actor(id, Value::Object(claims)));
            }
        }
        let header = jsonwebtoken::decode_header(token).ok()?;
        if header.alg == Algorithm::HS256 {
            let secret = self.jwt_secret.as_ref()?;
            let mut validation = Validation::new(Algorithm::HS256);
            validation.validate_aud = false;
            let data = jsonwebtoken::decode::<Value>(
                token,
                &DecodingKey::from_secret(secret.as_bytes()),
                &validation,
            )
            .ok()?;
            return self.actor_from_claims(data.claims);
        }
        let claims = self.oidc.as_ref()?.verify(token, &header)??;
        self.actor_from_claims(claims)
    }

    pub async fn from_headers(&self, headers: &HeaderMap) -> Option<Actor> {
        let value = headers.get("authorization")?.to_str().ok()?;
        let token = value
            .strip_prefix("Bearer ")
            .or_else(|| value.strip_prefix("bearer "))?
            .trim();
        if let Some(a) = self.from_token(token) {
            return Some(a);
        }
        // An unknown signing key: the IdP may have rotated its keys.
        let oidc = self.oidc.as_ref()?;
        let header = jsonwebtoken::decode_header(token).ok()?;
        if header.alg != Algorithm::HS256
            && oidc.verify(token, &header).is_none()
            && oidc.may_refresh()
        {
            if let Err(e) = oidc.refresh().await {
                tracing::warn!("OIDC refresh failed: {e}");
            }
            return self.from_token(token);
        }
        None
    }
}

pub const TICKET_TTL: Duration = Duration::from_secs(60);

#[derive(Default)]
pub struct Tickets {
    map: DashMap<String, (Actor, Instant)>,
}

impl Tickets {
    pub fn issue(&self, actor: Actor) -> String {
        self.map.retain(|_, (_, t)| t.elapsed() < TICKET_TTL);
        let ticket = uuid::Uuid::now_v7().simple().to_string();
        self.map.insert(ticket.clone(), (actor, Instant::now()));
        ticket
    }

    /// A ticket can be used once.
    pub fn redeem(&self, ticket: &str) -> Option<Actor> {
        let (_, (actor, issued)) = self.map.remove(ticket)?;
        (issued.elapsed() < TICKET_TTL).then_some(actor)
    }
}

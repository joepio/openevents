//! Who is calling. Tokens come from outside (requirement A7); the register has no login.
//!
//! - `Authorization: Bearer <JWT>`, HS256 with `OPENEVENTS_JWT_SECRET`. `sub` is the
//!   actor id; all claims are available to `actor('claim')` in rules.
//! - With `OPENEVENTS_DEV_AUTH=true` also `Bearer dev:<actor-id>|claim=value|...`, for
//!   demos and tests only.
//! - Actors listed in `OPENEVENTS_ADMINS` (comma separated) are administrators.
//! - For `EventSource` in browsers, which cannot send headers: `POST /events/tickets`
//!   returns a short-lived ticket for `GET /events?ticket=...`, so tokens stay out of URLs.

use std::time::{Duration, Instant};

use axum::http::HeaderMap;
use dashmap::DashMap;
use openevents_core::Actor;
use serde_json::{Map, Value};

#[derive(Clone, Default)]
pub struct AuthConfig {
    pub jwt_secret: Option<String>,
    pub dev_auth: bool,
    pub admins: Vec<String>,
}

impl AuthConfig {
    pub fn from_env() -> Self {
        AuthConfig {
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
        let secret = self.jwt_secret.as_ref()?;
        let mut validation = jsonwebtoken::Validation::new(jsonwebtoken::Algorithm::HS256);
        validation.validate_aud = false;
        let data = jsonwebtoken::decode::<Value>(
            token,
            &jsonwebtoken::DecodingKey::from_secret(secret.as_bytes()),
            &validation,
        )
        .ok()?;
        let id = data.claims.get("sub")?.as_str()?.to_string();
        Some(self.actor(id, data.claims))
    }

    pub fn from_headers(&self, headers: &HeaderMap) -> Option<Actor> {
        let value = headers.get("authorization")?.to_str().ok()?;
        let token = value
            .strip_prefix("Bearer ")
            .or_else(|| value.strip_prefix("bearer "))?;
        self.from_token(token.trim())
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

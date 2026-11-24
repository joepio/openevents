//! Pseudonyms for identifying numbers such as the BSN, so the register never stores them.
//!
//! With `OPENEVENTS_PSEUDONYM_KEY` set, a BSN becomes `pseudo:` plus an HMAC-SHA256 of
//! it with that key, in two places that meet in the rules:
//! - in the token: the claims in `OPENEVENTS_PSEUDONYM_CLAIMS` (default `bsn`), before
//!   any rule sees them, so `initiatorBsn = actor('bsn')` keeps working;
//! - in what the ZGW adapter mirrors: every `inpBsn` and `bsn` field.
//!
//! The same BSN gives the same pseudonym, so matching works; without the key the
//! pseudonym cannot be turned back into a BSN. Keep the key out of the data volume.

use ring::hmac;
use serde_json::Value;

pub struct Pseudonym {
    key: hmac::Key,
    claims: Vec<String>,
}

/// Field names whose values are pseudonymised in mirrored resources.
const FIELDS: &[&str] = &["inpBsn", "bsn"];

impl Pseudonym {
    pub fn new(key: &str, claims: Vec<String>) -> Self {
        Pseudonym {
            key: hmac::Key::new(hmac::HMAC_SHA256, key.as_bytes()),
            claims,
        }
    }

    pub fn from_env() -> Option<Self> {
        let key = std::env::var("OPENEVENTS_PSEUDONYM_KEY")
            .ok()
            .filter(|k| !k.is_empty())?;
        let claims = std::env::var("OPENEVENTS_PSEUDONYM_CLAIMS")
            .unwrap_or_else(|_| "bsn".into())
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        Some(Pseudonym::new(&key, claims))
    }

    pub fn of(&self, value: &str) -> String {
        if value.starts_with("pseudo:") {
            return value.to_string();
        }
        let tag = hmac::sign(&self.key, value.as_bytes());
        let hex: String = tag.as_ref().iter().map(|b| format!("{b:02x}")).collect();
        format!("pseudo:{hex}")
    }

    /// The token's claims, with the configured ones replaced by their pseudonym.
    pub fn claims(&self, claims: &mut Value) {
        for c in &self.claims {
            if let Some(v) = claims.get_mut(c) {
                self.value(v);
            }
        }
    }

    /// A mirrored resource, with every `inpBsn` and `bsn` replaced.
    pub fn resource(&self, v: &mut Value) {
        match v {
            Value::Object(m) => {
                for (k, x) in m.iter_mut() {
                    if FIELDS.contains(&k.as_str()) {
                        self.value(x);
                    } else {
                        self.resource(x);
                    }
                }
            }
            Value::Array(a) => a.iter_mut().for_each(|x| self.resource(x)),
            _ => {}
        }
    }

    fn value(&self, v: &mut Value) {
        match v {
            Value::String(s) if !s.is_empty() => *s = self.of(s),
            Value::Array(a) => a.iter_mut().for_each(|x| self.value(x)),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn same_bsn_same_pseudonym_and_no_bsn_left() {
        let p = Pseudonym::new("geheim", vec!["bsn".into()]);
        let mut rol = json!({ "betrokkeneIdentificatie": { "inpBsn": "999993653", "geslachtsnaam": "Moulin" } });
        p.resource(&mut rol);
        let mut claims = json!({ "sub": "x", "bsn": "999993653" });
        p.claims(&mut claims);
        assert_eq!(rol["betrokkeneIdentificatie"]["inpBsn"], claims["bsn"]);
        assert!(!rol.to_string().contains("999993653"));
        assert!(claims["bsn"].as_str().unwrap().starts_with("pseudo:"));
        // Applying twice changes nothing.
        let once = claims.clone();
        p.claims(&mut claims);
        assert_eq!(once, claims);
    }
}

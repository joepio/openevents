//! OpenEvents core: an event register with projections, profiles and CQL2 rules.
//!
//! See the design in `docs/` at the repository root.

pub mod cql2;
pub mod kv;
pub mod profile;
pub mod register;

pub use register::{
    Actor, EventFilter, QueryRequest, Register, RegisterError, StoredEvent, SubmitOk,
};

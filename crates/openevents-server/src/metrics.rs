//! `/metrics` in the Prometheus text format: enough to see that OpenEvents keeps up
//! (last event, notificaties, abonnement, backups, retention).

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

static NOTIF_STORED: AtomicU64 = AtomicU64::new(0);
static NOTIF_IGNORED: AtomicU64 = AtomicU64::new(0);
static NOTIF_FAILED: AtomicU64 = AtomicU64::new(0);
static NOTIF_LAST: AtomicU64 = AtomicU64::new(0);
static SUBSCRIBED_CHECK: AtomicU64 = AtomicU64::new(0);
static BACKUP_LAST: AtomicU64 = AtomicU64::new(0);
static PURGED: AtomicU64 = AtomicU64::new(0);

fn now() -> u64 {
    chrono::Utc::now().timestamp().max(0) as u64
}

pub fn notificatie(outcome: &str) {
    match outcome {
        "stored" => {
            NOTIF_STORED.fetch_add(1, Relaxed);
            NOTIF_LAST.store(now(), Relaxed);
        }
        "ignored" => {
            NOTIF_IGNORED.fetch_add(1, Relaxed);
        }
        _ => {
            NOTIF_FAILED.fetch_add(1, Relaxed);
        }
    }
}
pub fn subscription_ok() {
    SUBSCRIBED_CHECK.store(now(), Relaxed);
}
pub fn backup_done() {
    BACKUP_LAST.store(now(), Relaxed);
}
pub fn purged(n: usize) {
    PURGED.fetch_add(n as u64, Relaxed);
}

pub fn render(head: u64, last_event: Option<u64>, stream_clients: usize) -> String {
    let mut out = String::new();
    let mut m = |name: &str, kind: &str, help: &str, v: String| {
        out.push_str(&format!(
            "# HELP {name} {help}\n# TYPE {name} {kind}\n{name} {v}\n"
        ));
    };
    m(
        "openevents_events_total",
        "counter",
        "Events in the log (head sequence).",
        head.to_string(),
    );
    m(
        "openevents_last_event_timestamp_seconds",
        "gauge",
        "Time of the last event in the log.",
        last_event.unwrap_or(0).to_string(),
    );
    m(
        "openevents_stream_clients",
        "gauge",
        "Open event streams (SSE).",
        stream_clients.to_string(),
    );
    out.push_str("# HELP openevents_notificaties_total ZGW notificaties received, by outcome.\n# TYPE openevents_notificaties_total counter\n");
    for (o, v) in [
        ("stored", &NOTIF_STORED),
        ("ignored", &NOTIF_IGNORED),
        ("failed", &NOTIF_FAILED),
    ] {
        out.push_str(&format!(
            "openevents_notificaties_total{{outcome=\"{o}\"}} {}\n",
            v.load(Relaxed)
        ));
    }
    let mut m = |name: &str, kind: &str, help: &str, v: u64| {
        out.push_str(&format!(
            "# HELP {name} {help}\n# TYPE {name} {kind}\n{name} {v}\n"
        ));
    };
    m(
        "openevents_last_notificatie_timestamp_seconds",
        "gauge",
        "Last stored notificatie.",
        NOTIF_LAST.load(Relaxed),
    );
    m(
        "openevents_abonnement_checked_timestamp_seconds",
        "gauge",
        "Last successful check of the abonnement at Open Notificaties.",
        SUBSCRIBED_CHECK.load(Relaxed),
    );
    m(
        "openevents_last_backup_timestamp_seconds",
        "gauge",
        "Last successful backup.",
        BACKUP_LAST.load(Relaxed),
    );
    m(
        "openevents_purged_total",
        "counter",
        "Resources removed (retention or DELETE).",
        PURGED.load(Relaxed),
    );
    out
}

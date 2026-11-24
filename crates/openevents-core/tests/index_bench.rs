//! Indexes against a scan, with many taken. Run with
//! `cargo test --release -p openevents-core --test index_bench -- --ignored --nocapture`.

use std::sync::Arc;
use std::time::Instant;

use openevents_core::kv::RedbStore;
use openevents_core::{Actor, QueryRequest, Register};
use serde_json::{json, Value};

fn actor(id: &str, claims: Value, admin: bool) -> Actor {
    Actor {
        id: id.into(),
        claims,
        admin,
    }
}

#[tokio::test]
#[ignore]
async fn index_against_scan() {
    let n: usize = std::env::var("N")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(20_000);
    let dir = tempfile::tempdir().unwrap();
    let reg = Register::open(Arc::new(RedbStore::open(&dir.path().join("db")).unwrap())).unwrap();
    let admin = actor("urn:admin", json!({}), true);
    let raw: Value =
        serde_json::from_str(include_str!("../../../examples/profiles/taken.json")).unwrap();
    reg.put_profile(&admin, "nl.vng.taken", raw).unwrap();
    let systeem = actor("urn:sys", json!({ "rol": "systeem" }), false);
    let t0 = Instant::now();
    for chunk in 0..n / 100 {
        let commits: Vec<Value> = (0..100)
            .map(|i| {
                let k = chunk * 100 + i;
                json!({ "resourceId": format!("urn:taak:{k}"), "type": "nl.vng.taken.taak",
                    "resourceData": { "titel": format!("Taak {k}"), "voor": { "partij": format!("https://id.example.nl/p{}", k % 2000) }, "team": "t" } })
            })
            .collect();
        let ev = json!({ "specversion": "1.0", "id": format!("b{chunk}"), "source": "urn:bench", "type": "nl.vng.taken.taak.aangemaakt",
            "subject": format!("urn:taak:{}", chunk * 100), "data": { "commits": commits } });
        reg.submit(&systeem, ev).await.unwrap();
    }
    println!("wrote {n} taken in {:?}", t0.elapsed());
    let me = actor(
        "https://id.example.nl/p7",
        json!({ "rol": "inwoner" }),
        false,
    );
    let q = |filter: Option<Value>, mine: bool| QueryRequest {
        type_name: "nl.vng.taken.taak".into(),
        filter,
        fields: None,
        page_size: Some(100),
        cursor: None,
        mine,
    };
    for (label, req) in [
        ("mine (index)", q(None, true)),
        (
            "filter on voor.partij (index)",
            q(
                Some(json!("voor.partij = 'https://id.example.nl/p7'")),
                false,
            ),
        ),
        (
            "not team = x (scan)",
            q(
                Some(
                    json!({ "op": "not", "args": [ { "op": "=", "args": [ { "property": "team" }, "x" ] } ] }),
                ),
                false,
            ),
        ),
    ] {
        let t = Instant::now();
        let v = reg.query(&me, &req).unwrap();
        println!(
            "{label}: {} results in {:?}",
            v["results"].as_array().unwrap().len(),
            t.elapsed()
        );
    }
}

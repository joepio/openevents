//! Backups: the whole store as JSON lines (`{"k": key, "v": value}`), read in one
//! read transaction, so a backup is consistent. Restore loads such a file into an
//! empty store at start. Fits a register of a few GB in memory; larger needs a
//! streaming export.

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use openevents_core::kv::{KvStore, Op};
use serde_json::{json, Value};

const KEEP: usize = 7;

fn text(b: &[u8]) -> Result<&str, String> {
    std::str::from_utf8(b).map_err(|_| "store holds bytes that are not UTF-8".to_string())
}

/// Write a backup of `kv` to `path`. Returns the number of keys.
pub fn write(kv: &dyn KvStore, path: &Path) -> Result<usize, String> {
    let rows = kv
        .range(b"", &[0xff], usize::MAX)
        .map_err(|e| e.to_string())?;
    let tmp = path.with_extension("part");
    let mut f = std::io::BufWriter::new(std::fs::File::create(&tmp).map_err(|e| e.to_string())?);
    for (k, v) in &rows {
        let line = json!({ "k": text(k)?, "v": text(v)? });
        writeln!(f, "{line}").map_err(|e| e.to_string())?;
    }
    f.flush().map_err(|e| e.to_string())?;
    drop(f);
    std::fs::rename(&tmp, path).map_err(|e| e.to_string())?;
    Ok(rows.len())
}

/// Load a backup into `kv`, only when the store is empty. Returns the number of keys.
pub fn restore(kv: &dyn KvStore, path: &Path) -> Result<usize, String> {
    if !kv
        .range(b"", &[0xff], 1)
        .map_err(|e| e.to_string())?
        .is_empty()
    {
        return Err("the store is not empty; restore only into an empty data directory".into());
    }
    let f = std::io::BufReader::new(std::fs::File::open(path).map_err(|e| e.to_string())?);
    let (mut n, mut ops) = (0, Vec::new());
    for line in f.lines() {
        let line = line.map_err(|e| e.to_string())?;
        if line.trim().is_empty() {
            continue;
        }
        let row: Value = serde_json::from_str(&line).map_err(|e| e.to_string())?;
        let (Some(k), Some(v)) = (row["k"].as_str(), row["v"].as_str()) else {
            return Err(format!("line {}: expected {{k, v}}", n + 1));
        };
        ops.push(Op::Put(k.as_bytes().to_vec(), v.as_bytes().to_vec()));
        n += 1;
        if ops.len() >= 1000 {
            kv.apply_batch(std::mem::take(&mut ops))
                .map_err(|e| e.to_string())?;
        }
    }
    kv.apply_batch(ops).map_err(|e| e.to_string())?;
    Ok(n)
}

/// Every `every`, write `openevents-{time}.jsonl` into `dir` and keep the last seven.
pub async fn keep_backing_up(kv: Arc<dyn KvStore>, dir: PathBuf, every: Duration) {
    let mut tick = tokio::time::interval(every);
    loop {
        tick.tick().await;
        let name = format!(
            "openevents-{}.jsonl",
            chrono::Utc::now().format("%Y%m%dT%H%M%SZ")
        );
        let (kv, path) = (kv.clone(), dir.join(name));
        let done = tokio::task::spawn_blocking(move || write(kv.as_ref(), &path)).await;
        match done {
            Ok(Ok(n)) => {
                crate::metrics::backup_done();
                tracing::info!("backup: {n} keys");
            }
            Ok(Err(e)) => tracing::warn!("backup failed: {e}"),
            Err(e) => tracing::warn!("backup failed: {e}"),
        }
        let mut old: Vec<_> = std::fs::read_dir(&dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "jsonl"))
            .collect();
        old.sort();
        while old.len() > KEEP {
            let _ = std::fs::remove_file(old.remove(0));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openevents_core::kv::MemoryStore;

    #[test]
    fn backup_and_restore() {
        let a = MemoryStore::new();
        a.apply_batch(vec![
            Op::Put(b"log/1".to_vec(), r#"{"x":"é"}"#.as_bytes().to_vec()),
            Op::Put(b"type/t\ns".to_vec(), vec![]),
        ])
        .unwrap();
        let dir = std::env::temp_dir().join(format!("oe-backup-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("b.jsonl");
        assert_eq!(write(&a, &path).unwrap(), 2);
        let b = MemoryStore::new();
        assert_eq!(restore(&b, &path).unwrap(), 2);
        assert_eq!(b.get(b"type/t\ns").unwrap(), Some(vec![]));
        assert!(restore(&b, &path).is_err(), "never over existing data");
        std::fs::remove_dir_all(dir).unwrap();
    }
}

//! Key-value storage behind a trait (ADR-0002).
//!
//! The core only needs `get`, ordered scans and an atomic batch. Indexes and queries
//! live in the core, not in the store. A confirmed `apply_batch` must be durable.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::RwLock;

#[derive(Debug, thiserror::Error)]
#[error("storage error: {0}")]
pub struct KvError(pub String);

pub type KvResult<T> = Result<T, KvError>;

#[derive(Debug, Clone)]
pub enum Op {
    Put(Vec<u8>, Vec<u8>),
    Delete(Vec<u8>),
}

pub trait KvStore: Send + Sync {
    fn get(&self, key: &[u8]) -> KvResult<Option<Vec<u8>>>;
    /// Entries with `start <= key < end`, ascending, at most `limit`.
    fn range(&self, start: &[u8], end: &[u8], limit: usize) -> KvResult<Vec<(Vec<u8>, Vec<u8>)>>;
    /// The last entry with `start <= key < end`, if any.
    fn last_in_range(&self, start: &[u8], end: &[u8]) -> KvResult<Option<(Vec<u8>, Vec<u8>)>>;
    /// Apply all operations atomically and durably.
    fn apply_batch(&self, ops: Vec<Op>) -> KvResult<()>;
}

/// End of the key range for a prefix: the prefix with its last byte incremented.
pub fn prefix_end(prefix: &[u8]) -> Vec<u8> {
    let mut end = prefix.to_vec();
    while let Some(last) = end.pop() {
        if last < 0xff {
            end.push(last + 1);
            return end;
        }
    }
    vec![0xff; prefix.len() + 1]
}

/// In-memory store, for tests and throwaway demos.
#[derive(Default)]
pub struct MemoryStore {
    map: RwLock<BTreeMap<Vec<u8>, Vec<u8>>>,
}

impl MemoryStore {
    pub fn new() -> Self {
        Self::default()
    }
}

impl KvStore for MemoryStore {
    fn get(&self, key: &[u8]) -> KvResult<Option<Vec<u8>>> {
        Ok(self.map.read().unwrap().get(key).cloned())
    }

    fn range(&self, start: &[u8], end: &[u8], limit: usize) -> KvResult<Vec<(Vec<u8>, Vec<u8>)>> {
        Ok(self
            .map
            .read()
            .unwrap()
            .range(start.to_vec()..end.to_vec())
            .take(limit)
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect())
    }

    fn last_in_range(&self, start: &[u8], end: &[u8]) -> KvResult<Option<(Vec<u8>, Vec<u8>)>> {
        Ok(self
            .map
            .read()
            .unwrap()
            .range(start.to_vec()..end.to_vec())
            .next_back()
            .map(|(k, v)| (k.clone(), v.clone())))
    }

    fn apply_batch(&self, ops: Vec<Op>) -> KvResult<()> {
        let mut map = self.map.write().unwrap();
        for op in ops {
            match op {
                Op::Put(k, v) => {
                    map.insert(k, v);
                }
                Op::Delete(k) => {
                    map.remove(&k);
                }
            }
        }
        Ok(())
    }
}

const TABLE: redb::TableDefinition<&[u8], &[u8]> = redb::TableDefinition::new("kv");

/// Embedded store on redb. Writes use redb's default `Durability::Immediate`: a batch
/// is on disk before `apply_batch` returns (requirement K1).
pub struct RedbStore {
    db: redb::Database,
}

fn err(e: impl std::fmt::Display) -> KvError {
    KvError(e.to_string())
}

impl RedbStore {
    pub fn open(path: &Path) -> KvResult<Self> {
        let db = redb::Database::create(path).map_err(err)?;
        let tx = db.begin_write().map_err(err)?;
        tx.open_table(TABLE).map_err(err)?;
        tx.commit().map_err(err)?;
        Ok(Self { db })
    }
}

impl KvStore for RedbStore {
    fn get(&self, key: &[u8]) -> KvResult<Option<Vec<u8>>> {
        let tx = self.db.begin_read().map_err(err)?;
        let table = tx.open_table(TABLE).map_err(err)?;
        Ok(table.get(key).map_err(err)?.map(|v| v.value().to_vec()))
    }

    fn range(&self, start: &[u8], end: &[u8], limit: usize) -> KvResult<Vec<(Vec<u8>, Vec<u8>)>> {
        let tx = self.db.begin_read().map_err(err)?;
        let table = tx.open_table(TABLE).map_err(err)?;
        let mut out = Vec::new();
        for item in table.range(start..end).map_err(err)? {
            let (k, v) = item.map_err(err)?;
            out.push((k.value().to_vec(), v.value().to_vec()));
            if out.len() >= limit {
                break;
            }
        }
        Ok(out)
    }

    fn last_in_range(&self, start: &[u8], end: &[u8]) -> KvResult<Option<(Vec<u8>, Vec<u8>)>> {
        let tx = self.db.begin_read().map_err(err)?;
        let table = tx.open_table(TABLE).map_err(err)?;
        let mut iter = table.range(start..end).map_err(err)?;
        match iter.next_back() {
            Some(item) => {
                let (k, v) = item.map_err(err)?;
                Ok(Some((k.value().to_vec(), v.value().to_vec())))
            }
            None => Ok(None),
        }
    }

    fn apply_batch(&self, ops: Vec<Op>) -> KvResult<()> {
        let tx = self.db.begin_write().map_err(err)?;
        {
            let mut table = tx.open_table(TABLE).map_err(err)?;
            for op in &ops {
                match op {
                    Op::Put(k, v) => {
                        table.insert(k.as_slice(), v.as_slice()).map_err(err)?;
                    }
                    Op::Delete(k) => {
                        table.remove(k.as_slice()).map_err(err)?;
                    }
                }
            }
        }
        tx.commit().map_err(err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exercise(store: &dyn KvStore) {
        store
            .apply_batch(vec![
                Op::Put(b"a/1".to_vec(), b"x".to_vec()),
                Op::Put(b"a/2".to_vec(), b"y".to_vec()),
                Op::Put(b"b/1".to_vec(), b"z".to_vec()),
            ])
            .unwrap();
        assert_eq!(store.get(b"a/1").unwrap(), Some(b"x".to_vec()));
        let a = store.range(b"a/", &prefix_end(b"a/"), 10).unwrap();
        assert_eq!(a.len(), 2);
        let last = store
            .last_in_range(b"a/", &prefix_end(b"a/"))
            .unwrap()
            .unwrap();
        assert_eq!(last.0, b"a/2".to_vec());
        store
            .apply_batch(vec![Op::Delete(b"a/1".to_vec())])
            .unwrap();
        assert_eq!(store.get(b"a/1").unwrap(), None);
    }

    #[test]
    fn memory() {
        exercise(&MemoryStore::new());
    }

    #[test]
    fn redb() {
        let dir = tempfile::tempdir().unwrap();
        exercise(&RedbStore::open(&dir.path().join("t.redb")).unwrap());
    }
}

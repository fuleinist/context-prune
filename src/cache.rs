//! Content-hash cache of compressed blobs (SPEC v2 stretch goal).
//!
//! Identical payloads — the same `ls -R` dump, the same test output — show up
//! over and over in agent sessions. Hash the input, store the compressed
//! output, skip re-computation on repeat. Same SQLite file as stats, separate
//! table. F5 safety: any cache error degrades to plain compression.

use anyhow::Result;
use rusqlite::Connection;

pub struct CacheStore {
    conn: Connection,
}

impl CacheStore {
    pub fn open(path: &str) -> Result<Self> {
        let conn = Connection::open(path)?;
        conn.execute_batch(
            "PRAGMA busy_timeout = 2000;
             CREATE TABLE IF NOT EXISTS cache (
                hash TEXT PRIMARY KEY,
                ts TEXT NOT NULL DEFAULT (datetime('now')),
                input_len INTEGER NOT NULL,
                output_len INTEGER NOT NULL,
                payload BLOB NOT NULL
             );",
        )?;
        Ok(Self { conn })
    }

    pub fn get(&self, hash: &str) -> Result<Option<Vec<u8>>> {
        let mut stmt = self.conn.prepare("SELECT payload FROM cache WHERE hash = ?1")?;
        let mut rows = stmt.query(rusqlite::params![hash])?;
        match rows.next()? {
            Some(row) => Ok(Some(row.get(0)?)),
            None => Ok(None),
        }
    }

    pub fn put(&self, hash: &str, input_len: usize, output_len: usize, payload: &[u8]) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO cache (hash, input_len, output_len, payload) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![hash, input_len as i64, output_len as i64, payload],
        )?;
        Ok(())
    }

    pub fn entries(&self) -> Result<i64> {
        let count: i64 = self.conn.query_row("SELECT COUNT(*) FROM cache", [], |row| row.get(0))?;
        Ok(count)
    }
}

/// SHA-256 hex digest of a byte blob — the cache key.
pub fn hash_bytes(bytes: &[u8]) -> String {
    use sha2::Digest;
    let digest = sha2::Sha256::digest(bytes);
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_db() -> std::path::PathBuf {
        std::env::temp_dir().join(format!("ctx-prune-cache-test-{}.db", std::process::id()))
    }

    #[test]
    fn put_get_roundtrip() {
        let path = tmp_db();
        let c = CacheStore::open(path.to_str().unwrap()).unwrap();
        let h = hash_bytes(b"some tool output");
        assert!(c.get(&h).unwrap().is_none());
        c.put(&h, 16, 8, b"smaller!").unwrap();
        assert_eq!(c.get(&h).unwrap().unwrap(), b"smaller!");
        assert_eq!(c.entries().unwrap(), 1);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn hash_is_deterministic_and_distinct() {
        assert_eq!(hash_bytes(b"abc"), hash_bytes(b"abc"));
        assert_ne!(hash_bytes(b"abc"), hash_bytes(b"abd"));
        assert_eq!(hash_bytes(b"abc").len(), 64);
    }

    #[test]
    fn put_replaces_same_hash() {
        let path = tmp_db().with_extension("replace.db");
        let c = CacheStore::open(path.to_str().unwrap()).unwrap();
        let h = hash_bytes(b"dup");
        c.put(&h, 3, 2, b"v1").unwrap();
        c.put(&h, 3, 2, b"v2").unwrap();
        assert_eq!(c.get(&h).unwrap().unwrap(), b"v2");
        assert_eq!(c.entries().unwrap(), 1);
        let _ = std::fs::remove_file(&path);
    }
}

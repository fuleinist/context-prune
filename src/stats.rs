//! SQLite-backed stats persistence (F3).

use anyhow::Result;
use rusqlite::Connection;

#[derive(Debug, Default)]
pub struct Summary {
    pub requests: i64,
    pub bytes_in: i64,
    pub bytes_out: i64,
}

pub struct StatsStore {
    conn: Connection,
}

impl StatsStore {
    pub fn open(path: &str) -> Result<Self> {
        let conn = Connection::open(path)?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS requests (
                id INTEGER PRIMARY KEY,
                ts TEXT NOT NULL DEFAULT (datetime('now')),
                endpoint TEXT NOT NULL,
                bytes_in INTEGER NOT NULL,
                bytes_out INTEGER NOT NULL,
                saved INTEGER NOT NULL DEFAULT 0
            );",
        )?;
        Ok(Self { conn })
    }

    pub fn record(&self, endpoint: &str, bytes_in: i64, bytes_out: i64) -> Result<()> {
        self.conn.execute(
            "INSERT INTO requests (endpoint, bytes_in, bytes_out, saved) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![endpoint, bytes_in, bytes_out, bytes_in - bytes_out],
        )?;
        Ok(())
    }

    pub fn summary(&self) -> Result<Summary> {
        let mut stmt = self
            .conn
            .prepare("SELECT COUNT(*), COALESCE(SUM(bytes_in),0), COALESCE(SUM(bytes_out),0) FROM requests")?;
        let summary = stmt.query_row([], |row| {
            Ok(Summary {
                requests: row.get(0)?,
                bytes_in: row.get(1)?,
                bytes_out: row.get(2)?,
            })
        })?;
        Ok(summary)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_and_summarizes() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("ctx-prune-test-{}.db", std::process::id()));
        let s = StatsStore::open(path.to_str().unwrap()).unwrap();
        s.record("/v1/chat/completions", 10_000, 4_000).unwrap();
        s.record("/v1/chat/completions", 5_000, 4_500).unwrap();
        let sum = s.summary().unwrap();
        assert_eq!(sum.requests, 2);
        assert_eq!(sum.bytes_in, 15_000);
        assert_eq!(sum.bytes_out, 8_500);
        let _ = std::fs::remove_file(&path);
    }
}

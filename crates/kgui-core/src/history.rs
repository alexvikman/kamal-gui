//! Every kamal run the app starts: what, where, which commit, outcome, and
//! the full output (one log file per run).

use crate::settings::data_dir;
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Serialize)]
pub struct Run {
    pub id: i64,
    pub project: String,
    pub units: String,
    pub action: String,
    pub git_ref: Option<String>,
    pub sha: Option<String>,
    pub subject: Option<String>,
    pub started_at: i64,
    pub finished_at: Option<i64>,
    /// running | ok | failed | cancelled | interrupted
    pub status: String,
    pub exit_code: Option<i32>,
}

pub fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

pub fn log_path(id: i64) -> PathBuf {
    data_dir().join("logs").join(format!("{id}.log"))
}

pub struct History {
    conn: Connection,
}

impl History {
    pub fn open() -> Result<Self> {
        let dir = crate::settings::ensure_data_dir()?;
        std::fs::create_dir_all(dir.join("logs"))?;
        Self::open_at(&dir.join("history.sqlite"))
    }

    fn open_at(path: &std::path::Path) -> Result<Self> {
        let conn = Connection::open(path)?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             CREATE TABLE IF NOT EXISTS runs (
               id INTEGER PRIMARY KEY AUTOINCREMENT,
               project TEXT NOT NULL,
               units TEXT NOT NULL,
               action TEXT NOT NULL,
               git_ref TEXT, sha TEXT, subject TEXT,
               started_at INTEGER NOT NULL,
               finished_at INTEGER,
               status TEXT NOT NULL,
               exit_code INTEGER
             );
             CREATE INDEX IF NOT EXISTS runs_project ON runs(project, id DESC);
             CREATE TABLE IF NOT EXISTS deploys (
               project TEXT PRIMARY KEY,
               at INTEGER NOT NULL
             );",
        )?;
        Ok(Self { conn })
    }

    /// Runs left as `running` when the app quit can never finish.
    pub fn mark_interrupted(&self) -> Result<()> {
        self.conn.execute(
            "UPDATE runs SET status='interrupted', finished_at=?1 WHERE status='running'",
            params![now_ms()],
        )?;
        Ok(())
    }

    pub fn start(&self, project: &str, units: &str, action: &str) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO runs (project, units, action, started_at, status) VALUES (?1, ?2, ?3, ?4, 'running')",
            params![project, units, action, now_ms()],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn set_commit(&self, id: i64, git_ref: &str, sha: &str, subject: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE runs SET git_ref=?2, sha=?3, subject=?4 WHERE id=?1",
            params![id, git_ref, sha, subject],
        )?;
        Ok(())
    }

    pub fn finish(&self, id: i64, status: &str, exit_code: Option<i32>) -> Result<()> {
        self.conn.execute(
            "UPDATE runs SET status=?2, exit_code=?3, finished_at=?4 WHERE id=?1",
            params![id, status, exit_code, now_ms()],
        )?;
        Ok(())
    }

    /// A deploy seen on the servers (the newest app container's creation
    /// time), so deploys from the kamal CLI count too. Keeps the latest.
    pub fn record_deploy(&self, project: &str, at: i64) -> Result<()> {
        self.conn.execute(
            "INSERT INTO deploys (project, at) VALUES (?1, ?2)
             ON CONFLICT(project) DO UPDATE SET at = max(at, excluded.at)",
            params![project, at],
        )?;
        Ok(())
    }

    /// When each project was last deployed, as far as we know: from the
    /// servers, or from a successful deploy run in the app.
    pub fn last_deploys(&self) -> Result<HashMap<String, i64>> {
        let mut stmt = self.conn.prepare(
            "SELECT project, max(at) FROM (
               SELECT project, at FROM deploys
               UNION ALL
               SELECT project, finished_at FROM runs
                 WHERE action = 'deploy' AND status = 'ok' AND finished_at IS NOT NULL
             ) GROUP BY project",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn get(&self, id: i64) -> Result<Option<Run>> {
        Ok(self.conn.query_row(&format!("SELECT {COLS} FROM runs WHERE id=?1"), params![id], row).optional()?)
    }

    pub fn list(&self, project: Option<&str>, limit: u32) -> Result<Vec<Run>> {
        let mut stmt = self
            .conn
            .prepare(&format!("SELECT {COLS} FROM runs WHERE (?1 IS NULL OR project=?1) ORDER BY id DESC LIMIT ?2"))?;
        let rows = stmt.query_map(params![project, limit], row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
}

const COLS: &str = "id, project, units, action, git_ref, sha, subject, started_at, finished_at, status, exit_code";

fn row(r: &rusqlite::Row) -> rusqlite::Result<Run> {
    Ok(Run {
        id: r.get(0)?,
        project: r.get(1)?,
        units: r.get(2)?,
        action: r.get(3)?,
        git_ref: r.get(4)?,
        sha: r.get(5)?,
        subject: r.get(6)?,
        started_at: r.get(7)?,
        finished_at: r.get(8)?,
        status: r.get(9)?,
        exit_code: r.get(10)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn last_deploy_is_the_newest_seen() {
        let path = std::env::temp_dir().join(format!("kgui-history-{}.sqlite", std::process::id()));
        let h = History::open_at(&path).unwrap();
        h.record_deploy("a", 2_000).unwrap();
        h.record_deploy("a", 1_000).unwrap();
        let id = h.start("b", "web", "deploy").unwrap();
        h.finish(id, "ok", Some(0)).unwrap();
        let failed = h.start("c", "web", "deploy").unwrap();
        h.finish(failed, "failed", Some(1)).unwrap();
        let other = h.start("c", "web", "app logs").unwrap();
        h.finish(other, "ok", Some(0)).unwrap();
        let last = h.last_deploys().unwrap();
        assert_eq!(last.get("a"), Some(&2_000));
        assert!(last.get("b").is_some_and(|&t| t > 2_000));
        assert_eq!(last.get("c"), None);
        drop(h);
        for ext in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{ext}", path.display()));
        }
    }
}

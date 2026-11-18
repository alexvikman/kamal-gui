//! Every kamal run the app starts: what, where, which commit, outcome, and
//! the full output (one log file per run).

use crate::settings::data_dir;
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
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
        let dir = data_dir();
        std::fs::create_dir_all(dir.join("logs"))?;
        let conn = Connection::open(dir.join("history.sqlite"))?;
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
             CREATE INDEX IF NOT EXISTS runs_project ON runs(project, id DESC);",
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

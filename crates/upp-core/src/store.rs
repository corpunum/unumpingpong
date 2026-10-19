// SPDX-License-Identifier: Apache-2.0
//! Append-only SQLite store (WAL). Every rally and event is a new row that is
//! never updated, so the database is a replayable trace and training data.
//! The `tables` row is the only mutable projection (status, counters).
//!
//! The store is also the bus between processes: `upp run`, `uppd`, `upp mcp`
//! and `upp watch` share one database; a table is claimed atomically by one
//! scheduler, stop requests and external server answers are rows.

use crate::config::TableSpec;
use crate::notify;
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const TERMINAL: &[&str] = &["point", "lost", "expired", "stopped", "error", "skipped"];

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

pub fn new_id(prefix: &str) -> String {
    let u = uuid::Uuid::new_v4().simple().to_string();
    format!("{prefix}{}", &u[..10])
}

/// Default database path: `$UPP_DB`, else `./.upp/upp.db`.
pub fn default_db_path() -> PathBuf {
    std::env::var_os("UPP_DB")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(".upp").join("upp.db"))
}

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct TableRow {
    pub id: String,
    pub run_id: String,
    pub name: String,
    pub goal: String,
    pub status: String,
    pub phase: String,
    pub player: String,
    pub after_id: Option<String>,
    pub created_ms: i64,
    pub started_ms: Option<i64>,
    pub ended_ms: Option<i64>,
    pub rallies: u32,
    pub max_rallies: u32,
    pub wall_secs: u64,
    pub tokens: u64,
    pub cost_usd: f64,
    pub session: Option<String>,
    /// One char per rally: `.` returned, `!` fault (player error), `*` point.
    pub trail: String,
    pub current_ball: Option<String>,
    pub last_answer: Option<String>,
    pub result: Option<String>,
    pub reason: Option<String>,
    pub owner: Option<String>,
    pub stop_requested: bool,
    /// The table that opened this one (nested tables).
    pub parent_id: Option<String>,
    /// Nesting depth: 0 for a top-level table.
    pub depth: u32,
    /// Orchestrator-side tokens: a model server's or orchestrator's reported usage, or, for an
    /// external server, an estimate of what it read (cards, transcripts) and wrote (notes).
    pub server_tokens: u64,
    /// How often the table was resumed after its scheduler process died.
    pub resumes: u32,
    /// The table's git worktree, when it has one: {repo, path, branch, base}.
    pub worktree: Option<Value>,
}

impl TableRow {
    pub fn is_terminal(&self) -> bool {
        TERMINAL.contains(&self.status.as_str())
    }
    pub fn elapsed_ms(&self, now: i64) -> i64 {
        match (self.started_ms, self.ended_ms) {
            (Some(s), Some(e)) => e - s,
            (Some(s), None) => now - s,
            _ => 0,
        }
    }
}

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct RallyRow {
    pub id: i64,
    pub table_id: String,
    pub n: u32,
    pub ball: String,
    pub answer: String,
    pub checks: Value,
    pub passed: bool,
    /// `point`, `return` or `fault`.
    pub verdict: String,
    pub note: String,
    pub tokens_in: u64,
    pub tokens_out: u64,
    pub cost_usd: f64,
    pub session: Option<String>,
    pub started_ms: i64,
    pub ended_ms: i64,
    /// What the referee checked: {git, commit, tree, dirty, base, changed} (or {git: false}).
    pub provenance: Value,
}

/// A result card as stored: `card` is the JSON the orchestrator reads.
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct CardRow {
    pub id: i64,
    pub table_id: String,
    pub run_id: String,
    pub rally: u32,
    /// `rally` (after the referee) or `final` (the table ended).
    pub kind: String,
    pub card: Value,
    pub created_ms: i64,
}

/// A waiter's view of the store generation (see [`Store::wait`]).
#[derive(Debug, Clone, Copy)]
pub struct Watch {
    gen: u64,
    dv: i64,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct EventRow {
    pub id: i64,
    pub ts: i64,
    pub table_id: String,
    pub kind: String,
    pub data: Value,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct PendingBall {
    pub table_id: String,
    pub rally: u32,
    pub payload: Value,
    pub created_ms: i64,
}

pub struct Store {
    conn: Connection,
    path: PathBuf,
}

const SCHEMA: &str = r#"
BEGIN;
CREATE TABLE IF NOT EXISTS runs (
  id TEXT PRIMARY KEY, name TEXT, created_ms INTEGER NOT NULL, source TEXT
);
CREATE TABLE IF NOT EXISTS tables (
  id TEXT PRIMARY KEY, run_id TEXT NOT NULL, name TEXT NOT NULL, goal TEXT NOT NULL,
  spec_json TEXT NOT NULL, status TEXT NOT NULL, phase TEXT NOT NULL DEFAULT 'idle',
  player TEXT NOT NULL, after_id TEXT, created_ms INTEGER NOT NULL,
  started_ms INTEGER, ended_ms INTEGER, rallies INTEGER NOT NULL DEFAULT 0,
  max_rallies INTEGER NOT NULL, wall_secs INTEGER NOT NULL,
  tokens INTEGER NOT NULL DEFAULT 0, cost_usd REAL NOT NULL DEFAULT 0,
  session TEXT, trail TEXT NOT NULL DEFAULT '', current_ball TEXT, last_answer TEXT,
  result TEXT, reason TEXT, owner TEXT, stop_requested INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS tables_status ON tables(status);
CREATE TABLE IF NOT EXISTS rallies (
  id INTEGER PRIMARY KEY AUTOINCREMENT, table_id TEXT NOT NULL, n INTEGER NOT NULL,
  ball TEXT NOT NULL, answer TEXT NOT NULL, checks_json TEXT NOT NULL, passed INTEGER NOT NULL,
  verdict TEXT NOT NULL, note TEXT NOT NULL, tokens_in INTEGER NOT NULL, tokens_out INTEGER NOT NULL,
  cost_usd REAL NOT NULL, session TEXT, started_ms INTEGER NOT NULL, ended_ms INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS rallies_table ON rallies(table_id, n);
CREATE TABLE IF NOT EXISTS server_queue (
  table_id TEXT NOT NULL, rally INTEGER NOT NULL, payload_json TEXT NOT NULL,
  answer_json TEXT, created_ms INTEGER NOT NULL, answered_ms INTEGER,
  PRIMARY KEY (table_id, rally)
);
CREATE TABLE IF NOT EXISTS events (
  id INTEGER PRIMARY KEY AUTOINCREMENT, ts INTEGER NOT NULL, table_id TEXT NOT NULL,
  kind TEXT NOT NULL, data_json TEXT NOT NULL
);
COMMIT;
"#;

/// v0.2 additions; applied idempotently to stores created by v0.1.
const SCHEMA_V2: &str = r#"
CREATE TABLE IF NOT EXISTS cards (
  id INTEGER PRIMARY KEY AUTOINCREMENT, table_id TEXT NOT NULL, run_id TEXT NOT NULL,
  rally INTEGER NOT NULL, kind TEXT NOT NULL, card_json TEXT NOT NULL, created_ms INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS cards_table ON cards(table_id, id);
CREATE INDEX IF NOT EXISTS cards_run ON cards(run_id, id);
"#;

const V2_TABLE_COLUMNS: &[(&str, &str, &str)] = &[
    ("tables", "parent_id", "TEXT"),
    ("tables", "depth", "INTEGER NOT NULL DEFAULT 0"),
    ("tables", "server_tokens", "INTEGER NOT NULL DEFAULT 0"),
    ("tables", "resumes", "INTEGER NOT NULL DEFAULT 0"),
    ("tables", "worktree_json", "TEXT"),
    ("rallies", "provenance_json", "TEXT"),
];

const SCHEMA_VERSION: i64 = 2;

const TABLE_COLS: &str = "id, run_id, name, goal, status, phase, player, after_id, created_ms, \
    started_ms, ended_ms, rallies, max_rallies, wall_secs, tokens, cost_usd, session, trail, \
    current_ball, last_answer, result, reason, owner, stop_requested, parent_id, depth, \
    server_tokens, resumes, worktree_json";

fn table_from_row(r: &rusqlite::Row) -> rusqlite::Result<TableRow> {
    Ok(TableRow {
        id: r.get(0)?,
        run_id: r.get(1)?,
        name: r.get(2)?,
        goal: r.get(3)?,
        status: r.get(4)?,
        phase: r.get(5)?,
        player: r.get(6)?,
        after_id: r.get(7)?,
        created_ms: r.get(8)?,
        started_ms: r.get(9)?,
        ended_ms: r.get(10)?,
        rallies: r.get(11)?,
        max_rallies: r.get(12)?,
        wall_secs: r.get::<_, i64>(13)? as u64,
        tokens: r.get::<_, i64>(14)? as u64,
        cost_usd: r.get(15)?,
        session: r.get(16)?,
        trail: r.get(17)?,
        current_ball: r.get(18)?,
        last_answer: r.get(19)?,
        result: r.get(20)?,
        reason: r.get(21)?,
        owner: r.get(22)?,
        stop_requested: r.get::<_, i64>(23)? != 0,
        parent_id: r.get(24)?,
        depth: r.get(25)?,
        server_tokens: r.get::<_, i64>(26)? as u64,
        resumes: r.get(27)?,
        worktree: r
            .get::<_, Option<String>>(28)?
            .and_then(|s| serde_json::from_str(&s).ok()),
    })
}

fn rally_from_row(r: &rusqlite::Row) -> rusqlite::Result<RallyRow> {
    let checks: String = r.get(5)?;
    Ok(RallyRow {
        id: r.get(0)?,
        table_id: r.get(1)?,
        n: r.get(2)?,
        ball: r.get(3)?,
        answer: r.get(4)?,
        checks: serde_json::from_str(&checks).unwrap_or(Value::Null),
        passed: r.get::<_, i64>(6)? != 0,
        verdict: r.get(7)?,
        note: r.get(8)?,
        tokens_in: r.get::<_, i64>(9)? as u64,
        tokens_out: r.get::<_, i64>(10)? as u64,
        cost_usd: r.get(11)?,
        session: r.get(12)?,
        started_ms: r.get(13)?,
        ended_ms: r.get(14)?,
        provenance: r
            .get::<_, Option<String>>(15)?
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or(Value::Null),
    })
}

fn card_from_row(r: &rusqlite::Row) -> rusqlite::Result<CardRow> {
    let c: String = r.get(5)?;
    Ok(CardRow {
        id: r.get(0)?,
        table_id: r.get(1)?,
        run_id: r.get(2)?,
        rally: r.get(3)?,
        kind: r.get(4)?,
        card: serde_json::from_str(&c).unwrap_or(Value::Null),
        created_ms: r.get(6)?,
    })
}

const RALLY_COLS: &str = "id, table_id, n, ball, answer, checks_json, passed, verdict, note, \
    tokens_in, tokens_out, cost_usd, session, started_ms, ended_ms, provenance_json";

type R<T> = Result<T, String>;
fn e(x: rusqlite::Error) -> String {
    format!("store: {x}")
}

/// Bring a v0.1 store to the current schema (idempotent; one writer at a time).
fn migrate(conn: &Connection) -> R<()> {
    conn.execute_batch("BEGIN IMMEDIATE").map_err(e)?;
    let res = (|| {
        conn.execute_batch(SCHEMA_V2).map_err(e)?;
        for (table, col, decl) in V2_TABLE_COLUMNS {
            let have: i64 = conn
                .query_row(
                    &format!("SELECT COUNT(*) FROM pragma_table_info('{table}') WHERE name=?1"),
                    [col],
                    |r| r.get(0),
                )
                .map_err(e)?;
            if have == 0 {
                conn.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {col} {decl}"))
                    .map_err(e)?;
            }
        }
        conn.execute_batch(&format!("PRAGMA user_version = {SCHEMA_VERSION}"))
            .map_err(e)
    })();
    match res {
        Ok(()) => conn.execute_batch("COMMIT").map_err(e),
        Err(x) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(x)
        }
    }
}

/// Progress fields written after each rally.
#[derive(Debug, Clone, Default)]
pub struct Progress {
    pub rallies: u32,
    pub tokens: u64,
    pub cost_usd: f64,
    pub session: Option<String>,
    pub trail: String,
    pub last_answer: Option<String>,
}

impl Store {
    pub fn open(path: &Path) -> R<Store> {
        if let Some(dir) = path.parent() {
            if !dir.as_os_str().is_empty() {
                std::fs::create_dir_all(dir)
                    .map_err(|x| format!("cannot create {}: {x}", dir.display()))?;
            }
        }
        // Opening races with other processes (WAL setup, schema check) can report
        // "locked" without consulting the busy handler: retry for a while.
        let mut attempt = 0;
        loop {
            match Self::try_open(path) {
                Ok(s) => return Ok(s),
                Err(x) if attempt < 100 && (x.contains("locked") || x.contains("busy")) => {
                    attempt += 1;
                    std::thread::sleep(Duration::from_millis(20 + 5 * attempt));
                }
                Err(x) => return Err(x),
            }
        }
    }

    fn try_open(path: &Path) -> R<Store> {
        let conn = Connection::open(path).map_err(e)?;
        conn.busy_timeout(Duration::from_secs(15)).map_err(e)?;
        let mode: String = conn
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .map_err(e)?;
        if !mode.eq_ignore_ascii_case("wal") {
            conn.pragma_update(None, "journal_mode", "WAL").map_err(e)?;
        }
        conn.pragma_update(None, "synchronous", "NORMAL")
            .map_err(e)?;
        let have: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='events'",
                [],
                |r| r.get(0),
            )
            .map_err(e)?;
        if have == 0 {
            conn.execute_batch(SCHEMA).map_err(e)?;
        }
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .map_err(e)?;
        if version < SCHEMA_VERSION {
            migrate(&conn)?;
        }
        Ok(Store {
            conn,
            path: path.to_path_buf(),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// SQLite's `data_version`: changes when another connection committed.
    pub fn data_version(&self) -> i64 {
        self.conn
            .query_row("PRAGMA data_version", [], |r| r.get(0))
            .unwrap_or(0)
    }

    pub fn watch(&self) -> Watch {
        Watch {
            gen: notify::generation(),
            dv: self.data_version(),
        }
    }

    /// Block until the store changed since `w` (a write in this process wakes at once; a
    /// write from another process is seen within `notify::CROSS_PROCESS_CHECK`), or `max`
    /// passes. Returns whether something changed; `w` is updated.
    pub fn wait(&self, w: &mut Watch, max: Duration) -> bool {
        let until = std::time::Instant::now() + max;
        loop {
            let left = until.saturating_duration_since(std::time::Instant::now());
            let g = notify::wait_change(w.gen, left.min(notify::CROSS_PROCESS_CHECK));
            if g != w.gen {
                w.gen = g;
                w.dv = self.data_version();
                return true;
            }
            let dv = self.data_version();
            if dv != w.dv {
                w.dv = dv;
                return true;
            }
            if std::time::Instant::now() >= until {
                return false;
            }
        }
    }

    /// Register a run and its resolved tables (status `queued`). Returns (run_id, table ids).
    pub fn create_run(
        &self,
        name: Option<&str>,
        source: Option<&str>,
        tables: &[TableSpec],
    ) -> R<(String, Vec<String>)> {
        let run_id = new_id("r");
        self.conn
            .execute(
                "INSERT INTO runs (id, name, created_ms, source) VALUES (?1, ?2, ?3, ?4)",
                params![run_id, name, now_ms(), source],
            )
            .map_err(e)?;
        let ids = self.add_tables(&run_id, tables, None, None)?;
        Ok((run_id, ids))
    }

    /// Add resolved tables to an existing run (queued). `after` names resolve within the
    /// batch first, then against the run's tables; `after_id` chains every table of the
    /// batch that names no `after`; `parent` makes them sub-tables (depth = parent + 1).
    pub fn add_tables(
        &self,
        run_id: &str,
        tables: &[TableSpec],
        after_id: Option<&str>,
        parent: Option<&TableRow>,
    ) -> R<Vec<String>> {
        let now = now_ms();
        let tx = self.conn.unchecked_transaction().map_err(e)?;
        let ids: Vec<(String, String)> = tables
            .iter()
            .map(|t| (t.name.clone(), new_id("t")))
            .collect();
        for (i, t) in tables.iter().enumerate() {
            let after = match &t.after {
                Some(a) => match ids.iter().find(|(n, _)| n == a) {
                    Some((_, id)) => Some(id.clone()),
                    None => tx
                        .query_row(
                            "SELECT id FROM tables WHERE run_id=?1 AND name=?2 ORDER BY created_ms DESC LIMIT 1",
                            params![run_id, a],
                            |r| r.get::<_, String>(0),
                        )
                        .optional()
                        .map_err(e)?,
                },
                None => after_id.map(String::from),
            };
            let lease = t.lease.clone().unwrap_or_default();
            let spec_json = serde_json::to_string(t).map_err(|x| x.to_string())?;
            let player = t.player.as_ref().map(|p| p.label()).unwrap_or_default();
            tx.execute(
                "INSERT INTO tables (id, run_id, name, goal, spec_json, status, player, after_id, \
                 created_ms, max_rallies, wall_secs, parent_id, depth) \
                 VALUES (?1,?2,?3,?4,?5,'queued',?6,?7,?8,?9,?10,?11,?12)",
                params![
                    ids[i].1,
                    run_id,
                    t.name,
                    t.goal,
                    spec_json,
                    player,
                    after,
                    now + i as i64,
                    lease.rallies(),
                    lease.wall() as i64,
                    parent.map(|p| p.id.clone()),
                    parent.map(|p| p.depth + 1).unwrap_or(0)
                ],
            )
            .map_err(e)?;
            tx.execute(
                "INSERT INTO events (ts, table_id, kind, data_json) VALUES (?1, ?2, 'queued', ?3)",
                params![
                    now,
                    ids[i].1,
                    json!({"name": t.name, "run": run_id, "parent": parent.map(|p| &p.id)})
                        .to_string()
                ],
            )
            .map_err(e)?;
        }
        tx.commit().map_err(e)?;
        notify::bump();
        Ok(ids.into_iter().map(|(_, id)| id).collect())
    }

    pub fn spec(&self, id: &str) -> R<TableSpec> {
        let s: String = self
            .conn
            .query_row("SELECT spec_json FROM tables WHERE id = ?1", [id], |r| {
                r.get(0)
            })
            .map_err(e)?;
        serde_json::from_str(&s).map_err(|x| format!("bad spec for {id}: {x}"))
    }

    /// Mark queued tables whose upstream ended without a point (transitively) as `skipped`,
    /// and queued tables with a stop request as `stopped`.
    pub fn settle_queue(&self) -> R<()> {
        let now = now_ms();
        self.conn
            .execute(
                "UPDATE tables SET status='stopped', ended_ms=?1, reason='stopped before start' \
                 WHERE status='queued' AND stop_requested=1",
                [now],
            )
            .map_err(e)?;
        loop {
            let n = self
                .conn
                .execute(
                    "UPDATE tables SET status='skipped', ended_ms=?1, \
                     reason='upstream table ended without a point' \
                     WHERE status='queued' AND after_id IN (SELECT id FROM tables WHERE status IN \
                     ('lost','expired','stopped','error','skipped'))",
                    [now],
                )
                .map_err(e)?;
            if n == 0 {
                break;
            }
        }
        Ok(())
    }

    /// Atomically claim up to `limit` ready tables (queued, upstream scored). `run` filters by run id.
    pub fn claim(&self, owner: &str, limit: usize, run: Option<&str>) -> R<Vec<TableRow>> {
        if limit == 0 {
            return Ok(vec![]);
        }
        let ids: Vec<String> = {
            let mut st = self
                .conn
                .prepare(
                    "SELECT t.id FROM tables t WHERE t.status='queued' AND (?1 IS NULL OR t.run_id=?1) \
                     AND (t.after_id IS NULL OR EXISTS (SELECT 1 FROM tables u WHERE u.id=t.after_id \
                     AND u.status='point')) ORDER BY t.created_ms, t.rowid LIMIT ?2",
                )
                .map_err(e)?;
            let rows = st
                .query_map(params![run, limit as i64], |r| r.get(0))
                .map_err(e)?;
            rows.filter_map(Result::ok).collect()
        };
        let mut out = Vec::new();
        for id in ids {
            let n = self
                .conn
                .execute(
                    "UPDATE tables SET status='running', phase='idle', owner=?1, \
                     started_ms=COALESCE(started_ms, ?2) WHERE id=?3 AND status='queued'",
                    params![owner, now_ms(), id],
                )
                .map_err(e)?;
            if n == 1 {
                self.event(&id, "started", json!({"owner": owner}))?;
                if let Some(t) = self.table(&id)? {
                    out.push(t);
                }
            }
        }
        Ok(out)
    }

    pub fn table(&self, id: &str) -> R<Option<TableRow>> {
        self.conn
            .query_row(
                &format!("SELECT {TABLE_COLS} FROM tables WHERE id = ?1"),
                [id],
                table_from_row,
            )
            .optional()
            .map_err(e)
    }

    /// Resolve a table id or a unique name (the newest table with that name wins).
    pub fn resolve(&self, id_or_name: &str) -> R<TableRow> {
        if let Some(t) = self.table(id_or_name)? {
            return Ok(t);
        }
        self.conn
            .query_row(
                &format!(
                    "SELECT {TABLE_COLS} FROM tables WHERE name = ?1 ORDER BY created_ms DESC LIMIT 1"
                ),
                [id_or_name],
                table_from_row,
            )
            .optional()
            .map_err(e)?
            .ok_or_else(|| format!("no table '{id_or_name}'"))
    }

    /// Tables, newest run first. `run` filters by run id; `active_only` hides terminal tables.
    pub fn tables(&self, run: Option<&str>, active_only: bool) -> R<Vec<TableRow>> {
        let mut st = self
            .conn
            .prepare(&format!(
                "SELECT {TABLE_COLS} FROM tables WHERE (?1 IS NULL OR run_id = ?1) \
                 ORDER BY created_ms DESC, rowid DESC LIMIT 500"
            ))
            .map_err(e)?;
        let rows = st.query_map(params![run], table_from_row).map_err(e)?;
        let mut v: Vec<TableRow> = rows.filter_map(Result::ok).collect();
        if active_only {
            v.retain(|t| !t.is_terminal());
        }
        Ok(v)
    }

    pub fn latest_run(&self) -> R<Option<String>> {
        self.conn
            .query_row(
                "SELECT id FROM runs ORDER BY created_ms DESC, rowid DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .optional()
            .map_err(e)
    }

    pub fn set_phase(&self, id: &str, phase: &str, ball: Option<&str>) -> R<()> {
        match ball {
            Some(b) => self.conn.execute(
                "UPDATE tables SET phase=?1, current_ball=?2 WHERE id=?3",
                params![phase, b, id],
            ),
            None => self
                .conn
                .execute("UPDATE tables SET phase=?1 WHERE id=?2", params![phase, id]),
        }
        .map_err(e)?;
        Ok(())
    }

    pub fn set_status(&self, id: &str, status: &str) -> R<()> {
        self.conn
            .execute(
                "UPDATE tables SET status=?1 WHERE id=?2",
                params![status, id],
            )
            .map_err(e)?;
        Ok(())
    }

    pub fn progress(&self, id: &str, p: &Progress) -> R<()> {
        self.conn
            .execute(
                "UPDATE tables SET rallies=?1, tokens=?2, cost_usd=?3, session=COALESCE(?4, session), \
                 trail=?5, last_answer=COALESCE(?6, last_answer) WHERE id=?7",
                params![
                    p.rallies,
                    p.tokens as i64,
                    p.cost_usd,
                    p.session,
                    p.trail,
                    p.last_answer,
                    id
                ],
            )
            .map_err(e)?;
        Ok(())
    }

    /// End a table: terminal `status`, optional result (the point's answer, without its
    /// card block) and reason. Appends the table's `final` card.
    pub fn finish(&self, id: &str, status: &str, result: Option<&str>, reason: &str) -> R<()> {
        let result = result.map(crate::card::strip);
        self.conn
            .execute(
                "UPDATE tables SET status=?1, phase='idle', ended_ms=?2, result=?3, reason=?4 WHERE id=?5",
                params![status, now_ms(), result, reason, id],
            )
            .map_err(e)?;
        if let Some(t) = self.table(id)? {
            let mut c = self
                .latest_card(id, "rally")?
                .map(|r| r.card)
                .unwrap_or_else(|| json!({"table_id": id, "table": t.name, "done": false,
                    "summary": reason, "evidence": [], "open_issues": [], "artifacts": [], "source": "engine"}));
            c["kind"] = json!("final");
            c["status"] = json!(status);
            c["reason"] = json!(reason);
            c["rallies"] = json!(t.rallies);
            c["needs_answer"] = json!(false);
            c["table_tokens"] = json!({"player": t.tokens, "orchestrator": t.server_tokens});
            c["parent_id"] = json!(t.parent_id);
            self.append_card(id, &t.run_id, t.rallies, "final", &c)?;
        }
        self.event(id, status, json!({"reason": reason}))
    }

    /// Store a card. Returns its id (the cursor for `cards_since`).
    pub fn append_card(
        &self,
        table_id: &str,
        run_id: &str,
        rally: u32,
        kind: &str,
        card: &Value,
    ) -> R<i64> {
        self.conn
            .execute(
                "INSERT INTO cards (table_id, run_id, rally, kind, card_json, created_ms) VALUES (?1,?2,?3,?4,?5,?6)",
                params![table_id, run_id, rally, kind, card.to_string(), now_ms()],
            )
            .map_err(e)?;
        let id = self.conn.last_insert_rowid();
        notify::bump();
        Ok(id)
    }

    /// Publish a rally card that needs the external server's answer, together with its
    /// pending ball, in one transaction (no reader sees one without the other). Sets the
    /// payload's `card_id` and the table's status `awaiting_server`. Returns the card id.
    pub fn publish_for_server(
        &self,
        table_id: &str,
        run_id: &str,
        rally: u32,
        card: &Value,
        payload: &mut Value,
    ) -> R<i64> {
        let tx = self.conn.unchecked_transaction().map_err(e)?;
        let now = now_ms();
        tx.execute(
            "INSERT INTO cards (table_id, run_id, rally, kind, card_json, created_ms) VALUES (?1,?2,?3,'rally',?4,?5)",
            params![table_id, run_id, rally, card.to_string(), now],
        )
        .map_err(e)?;
        let id = tx.last_insert_rowid();
        payload["card_id"] = json!(id);
        tx.execute(
            "INSERT OR REPLACE INTO server_queue (table_id, rally, payload_json, created_ms) VALUES (?1,?2,?3,?4)",
            params![table_id, rally, payload.to_string(), now],
        )
        .map_err(e)?;
        tx.execute(
            "UPDATE tables SET status='awaiting_server', phase='server' WHERE id=?1",
            [table_id],
        )
        .map_err(e)?;
        tx.commit().map_err(e)?;
        notify::bump();
        Ok(id)
    }

    pub fn latest_card(&self, table_id: &str, kind: &str) -> R<Option<CardRow>> {
        self.conn
            .query_row(
                "SELECT id, table_id, run_id, rally, kind, card_json, created_ms FROM cards \
                 WHERE table_id=?1 AND kind=?2 ORDER BY id DESC LIMIT 1",
                params![table_id, kind],
                card_from_row,
            )
            .optional()
            .map_err(e)
    }

    pub fn cards_of(&self, table_id: &str) -> R<Vec<CardRow>> {
        let mut st = self
            .conn
            .prepare(
                "SELECT id, table_id, run_id, rally, kind, card_json, created_ms FROM cards \
                 WHERE table_id=?1 ORDER BY id",
            )
            .map_err(e)?;
        let rows = st.query_map([table_id], card_from_row).map_err(e)?;
        Ok(rows.filter_map(Result::ok).collect())
    }

    /// Cards with id > `since`, oldest first, optionally of one run.
    pub fn cards_since(&self, run: Option<&str>, since: i64, limit: usize) -> R<Vec<CardRow>> {
        let mut st = self
            .conn
            .prepare(
                "SELECT id, table_id, run_id, rally, kind, card_json, created_ms FROM cards \
                 WHERE id > ?1 AND (?2 IS NULL OR run_id = ?2) ORDER BY id LIMIT ?3",
            )
            .map_err(e)?;
        let rows = st
            .query_map(params![since, run, limit as i64], card_from_row)
            .map_err(e)?;
        Ok(rows.filter_map(Result::ok).collect())
    }

    pub fn last_card_id(&self) -> R<i64> {
        self.conn
            .query_row("SELECT COALESCE(MAX(id), 0) FROM cards", [], |r| r.get(0))
            .map_err(e)
    }

    /// Sub-tables opened by `parent_id`, oldest first.
    pub fn children(&self, parent_id: &str) -> R<Vec<TableRow>> {
        let mut st = self
            .conn
            .prepare(&format!(
                "SELECT {TABLE_COLS} FROM tables WHERE parent_id = ?1 ORDER BY created_ms, rowid"
            ))
            .map_err(e)?;
        let rows = st.query_map([parent_id], table_from_row).map_err(e)?;
        Ok(rows.filter_map(Result::ok).collect())
    }

    pub fn add_server_tokens(&self, id: &str, n: u64) -> R<()> {
        if n == 0 {
            return Ok(());
        }
        self.conn
            .execute(
                "UPDATE tables SET server_tokens = server_tokens + ?1 WHERE id=?2",
                params![n as i64, id],
            )
            .map_err(e)?;
        Ok(())
    }

    pub fn set_worktree(&self, id: &str, w: &Value) -> R<()> {
        self.conn
            .execute(
                "UPDATE tables SET worktree_json=?1 WHERE id=?2",
                params![w.to_string(), id],
            )
            .map_err(e)?;
        Ok(())
    }

    /// Tables of `owner` in `phase` (the scheduler frees the slots of tables waiting on sub-tables).
    pub fn count_in_phase(&self, owner: &str, phase: &str) -> R<usize> {
        self.conn
            .query_row(
                "SELECT COUNT(*) FROM tables WHERE owner=?1 AND phase=?2 AND status IN ('running','awaiting_server')",
                params![owner, phase],
                |r| r.get::<_, i64>(0),
            )
            .map(|n| n as usize)
            .map_err(e)
    }

    /// Crash recovery: put the running tables of a dead scheduler back in the queue. The
    /// runner that claims one resumes it after its last recorded rally.
    pub fn requeue(&self, owner_prefix: &str) -> R<usize> {
        let ids: Vec<String> = {
            let mut st = self
                .conn
                .prepare(
                    "SELECT id FROM tables WHERE status IN ('running','awaiting_server') AND owner LIKE ?1",
                )
                .map_err(e)?;
            let rows = st
                .query_map([format!("{owner_prefix}%")], |r| r.get(0))
                .map_err(e)?;
            rows.filter_map(Result::ok).collect()
        };
        for id in &ids {
            self.conn
                .execute(
                    "UPDATE tables SET status='queued', phase='idle', owner=NULL, resumes=resumes+1 \
                     WHERE id=?1 AND status IN ('running','awaiting_server')",
                    [id],
                )
                .map_err(e)?;
            self.event(id, "requeued", json!({"reason": "its scheduler process died; resumes after the last recorded rally"}))?;
        }
        Ok(ids.len())
    }

    pub fn append_rally(&self, r: &RallyRow) -> R<i64> {
        self.conn
            .execute(
                "INSERT INTO rallies (table_id, n, ball, answer, checks_json, passed, verdict, note, \
                 tokens_in, tokens_out, cost_usd, session, started_ms, ended_ms, provenance_json) \
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15)",
                params![
                    r.table_id,
                    r.n,
                    r.ball,
                    r.answer,
                    r.checks.to_string(),
                    r.passed as i64,
                    r.verdict,
                    r.note,
                    r.tokens_in as i64,
                    r.tokens_out as i64,
                    r.cost_usd,
                    r.session,
                    r.started_ms,
                    r.ended_ms,
                    if r.provenance.is_null() {
                        None
                    } else {
                        Some(r.provenance.to_string())
                    }
                ],
            )
            .map_err(e)?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn rallies(&self, table_id: &str) -> R<Vec<RallyRow>> {
        let mut st = self
            .conn
            .prepare(&format!(
                "SELECT {RALLY_COLS} FROM rallies WHERE table_id=?1 ORDER BY n, id"
            ))
            .map_err(e)?;
        let rows = st.query_map([table_id], rally_from_row).map_err(e)?;
        Ok(rows.filter_map(Result::ok).collect())
    }

    /// All rallies (optionally of one run), oldest first: the export / training-data view.
    pub fn all_rallies(&self, run: Option<&str>) -> R<Vec<RallyRow>> {
        let mut st = self
            .conn
            .prepare(
                "SELECT r.id, r.table_id, r.n, r.ball, r.answer, r.checks_json, r.passed, r.verdict, r.note, \
                 r.tokens_in, r.tokens_out, r.cost_usd, r.session, r.started_ms, r.ended_ms, r.provenance_json \
                 FROM rallies r JOIN tables t ON t.id = r.table_id WHERE (?1 IS NULL OR t.run_id = ?1) ORDER BY r.id",
            )
            .map_err(e)?;
        let rows = st.query_map(params![run], rally_from_row).map_err(e)?;
        Ok(rows.filter_map(Result::ok).collect())
    }

    pub fn event(&self, table_id: &str, kind: &str, data: Value) -> R<()> {
        self.conn
            .execute(
                "INSERT INTO events (ts, table_id, kind, data_json) VALUES (?1, ?2, ?3, ?4)",
                params![now_ms(), table_id, kind, data.to_string()],
            )
            .map_err(e)?;
        notify::bump();
        Ok(())
    }

    pub fn events_since(&self, after_id: i64, limit: usize) -> R<Vec<EventRow>> {
        let mut st = self
            .conn
            .prepare(
                "SELECT id, ts, table_id, kind, data_json FROM events WHERE id > ?1 ORDER BY id LIMIT ?2",
            )
            .map_err(e)?;
        let rows = st
            .query_map(params![after_id, limit as i64], |r| {
                let d: String = r.get(4)?;
                Ok(EventRow {
                    id: r.get(0)?,
                    ts: r.get(1)?,
                    table_id: r.get(2)?,
                    kind: r.get(3)?,
                    data: serde_json::from_str(&d).unwrap_or(Value::Null),
                })
            })
            .map_err(e)?;
        Ok(rows.filter_map(Result::ok).collect())
    }

    pub fn last_event_id(&self) -> R<i64> {
        self.conn
            .query_row("SELECT COALESCE(MAX(id), 0) FROM events", [], |r| r.get(0))
            .map_err(e)
    }

    /// Ask for a table (and its open sub-tables) to stop. A queued table stops at once; a
    /// running one is cancelled: its player's process group is killed.
    pub fn request_stop(&self, id: &str) -> R<()> {
        let n = self
            .conn
            .execute("UPDATE tables SET stop_requested=1 WHERE id=?1", [id])
            .map_err(e)?;
        if n == 0 {
            return Err(format!("no table '{id}'"));
        }
        self.event(id, "stop_requested", json!({}))?;
        for c in self.children(id)? {
            if !c.is_terminal() && !c.stop_requested {
                self.request_stop(&c.id)?;
            }
        }
        self.settle_queue()
    }

    pub fn stop_requested(&self, id: &str) -> R<bool> {
        self.conn
            .query_row("SELECT stop_requested FROM tables WHERE id=?1", [id], |r| {
                r.get::<_, i64>(0)
            })
            .map(|v| v != 0)
            .map_err(e)
    }

    /// Publish a ball for the external server.
    pub fn push_pending(&self, table_id: &str, rally: u32, payload: &Value) -> R<()> {
        self.conn
            .execute(
                "INSERT OR REPLACE INTO server_queue (table_id, rally, payload_json, created_ms) VALUES (?1,?2,?3,?4)",
                params![table_id, rally, payload.to_string(), now_ms()],
            )
            .map_err(e)?;
        notify::bump();
        Ok(())
    }

    pub fn take_answer(&self, table_id: &str, rally: u32) -> R<Option<Value>> {
        let a: Option<Option<String>> = self
            .conn
            .query_row(
                "SELECT answer_json FROM server_queue WHERE table_id=?1 AND rally=?2",
                params![table_id, rally],
                |r| r.get(0),
            )
            .optional()
            .map_err(e)?;
        Ok(a.flatten().and_then(|s| serde_json::from_str(&s).ok()))
    }

    /// Answer the table's open ball. Errors when the table is not waiting for its server.
    pub fn answer(&self, table_id: &str, answer: &Value) -> R<u32> {
        let rally: Option<u32> = self
            .conn
            .query_row(
                "SELECT rally FROM server_queue WHERE table_id=?1 AND answer_json IS NULL ORDER BY rally DESC LIMIT 1",
                [table_id],
                |r| r.get(0),
            )
            .optional()
            .map_err(e)?;
        let rally = rally
            .ok_or_else(|| format!("table '{table_id}' has no ball waiting for its server"))?;
        self.conn
            .execute(
                "UPDATE server_queue SET answer_json=?1, answered_ms=?2 WHERE table_id=?3 AND rally=?4",
                params![answer.to_string(), now_ms(), table_id, rally],
            )
            .map_err(e)?;
        notify::bump();
        Ok(rally)
    }

    pub fn pending(&self, table_id: Option<&str>) -> R<Vec<PendingBall>> {
        let mut st = self
            .conn
            .prepare(
                "SELECT q.table_id, q.rally, q.payload_json, q.created_ms FROM server_queue q \
                 JOIN tables t ON t.id = q.table_id WHERE q.answer_json IS NULL \
                 AND t.status = 'awaiting_server' AND (?1 IS NULL OR q.table_id = ?1) ORDER BY q.created_ms",
            )
            .map_err(e)?;
        let rows = st
            .query_map(params![table_id], |r| {
                let p: String = r.get(2)?;
                Ok(PendingBall {
                    table_id: r.get(0)?,
                    rally: r.get(1)?,
                    payload: serde_json::from_str(&p).unwrap_or(Value::Null),
                    created_ms: r.get(3)?,
                })
            })
            .map_err(e)?;
        Ok(rows.filter_map(Result::ok).collect())
    }

    /// Owners of running tables (for dead-process detection).
    pub fn running_owners(&self) -> R<Vec<String>> {
        let mut st = self
            .conn
            .prepare(
                "SELECT DISTINCT owner FROM tables WHERE status IN ('running','awaiting_server') AND owner IS NOT NULL",
            )
            .map_err(e)?;
        let rows = st.query_map([], |r| r.get(0)).map_err(e)?;
        Ok(rows.filter_map(Result::ok).collect())
    }

    /// Count of tables in a run that are not terminal.
    pub fn open_count(&self, run: Option<&str>) -> R<usize> {
        self.conn
            .query_row(
                "SELECT COUNT(*) FROM tables WHERE (?1 IS NULL OR run_id=?1) AND status NOT IN \
                 ('point','lost','expired','stopped','error','skipped')",
                params![run],
                |r| r.get::<_, i64>(0),
            )
            .map(|n| n as usize)
            .map_err(e)
    }

    /// Mark running tables whose owner starts with `owner_prefix` as `stopped`
    /// (their scheduler process ended or was interrupted).
    pub fn orphan(&self, owner_prefix: &str) -> R<usize> {
        let ids: Vec<String> = {
            let mut st = self
                .conn
                .prepare(
                    "SELECT id FROM tables WHERE status IN ('running','awaiting_server') AND owner LIKE ?1",
                )
                .map_err(e)?;
            let rows = st
                .query_map([format!("{owner_prefix}%")], |r| r.get(0))
                .map_err(e)?;
            rows.filter_map(Result::ok).collect()
        };
        for id in &ids {
            self.finish(
                id,
                "stopped",
                None,
                "interrupted: its scheduler process ended",
            )?;
        }
        Ok(ids.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Cmd, MatchFile};

    fn specs() -> Vec<TableSpec> {
        let m = MatchFile::parse(
            r#"
[defaults]
unverified = true
player = { kind = "shell", command = "cat" }
[[table]]
name = "a"
ball = "x"
[[table]]
name = "b"
after = "a"
ball = "y"
[[table]]
name = "c"
after = "b"
ball = "z"
"#,
        )
        .unwrap();
        m.resolve().unwrap()
    }

    #[test]
    fn create_claim_rally_roundtrip() {
        let d = tempfile::tempdir().unwrap();
        let s = Store::open(&d.path().join("x/upp.db")).unwrap();
        let (run, ids) = s.create_run(Some("t"), None, &specs()).unwrap();
        assert_eq!(ids.len(), 3);
        // Only `a` is ready: b and c wait on their upstream.
        let got = s.claim("me", 10, Some(&run)).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].name, "a");
        // A second claimer gets nothing.
        assert!(s.claim("other", 10, Some(&run)).unwrap().is_empty());
        let r = RallyRow {
            id: 0,
            table_id: ids[0].clone(),
            n: 1,
            ball: "x".into(),
            answer: "y".into(),
            checks: json!([{"pass": true}]),
            passed: true,
            verdict: "point".into(),
            note: String::new(),
            tokens_in: 3,
            tokens_out: 4,
            cost_usd: 0.0,
            session: Some("s".into()),
            started_ms: 1,
            ended_ms: 2,
            provenance: json!({"git": false}),
        };
        s.append_rally(&r).unwrap();
        s.finish(&ids[0], "point", Some("y"), "referee passed")
            .unwrap();
        let rs = s.rallies(&ids[0]).unwrap();
        assert_eq!(rs.len(), 1);
        assert_eq!(rs[0].checks, json!([{"pass": true}]));
        assert_eq!(rs[0].provenance, json!({"git": false}));
        // finish() appended the table's final card.
        let fc = s.latest_card(&ids[0], "final").unwrap().unwrap();
        assert_eq!(fc.card["status"], "point");
        // Now b is ready.
        let got = s.claim("me", 10, Some(&run)).unwrap();
        assert_eq!(got[0].name, "b");
        assert!(s.events_since(0, 100).unwrap().len() >= 5);
        assert_eq!(s.spec(&ids[1]).unwrap().after.as_deref(), Some("a"));
        let _ = Cmd::Line("x".into());
    }

    #[test]
    fn a_v01_store_is_migrated_in_place() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("old.db");
        {
            let c = Connection::open(&path).unwrap();
            c.execute_batch(SCHEMA).unwrap();
            c.execute(
                "INSERT INTO tables (id, run_id, name, goal, spec_json, status, player, created_ms, \
                 max_rallies, wall_secs) VALUES ('t1','r1','old','g','{}','point','shell',1,5,60)",
                [],
            )
            .unwrap();
        }
        let s = Store::open(&path).unwrap();
        let t = s.table("t1").unwrap().unwrap();
        assert_eq!((t.depth, t.server_tokens, t.resumes), (0, 0, 0));
        assert!(t.parent_id.is_none());
        assert!(s.cards_of("t1").unwrap().is_empty());
        // Opening again is a no-op.
        drop(s);
        Store::open(&path).unwrap();
    }

    #[test]
    fn sub_tables_requeue_and_cascading_stop() {
        let d = tempfile::tempdir().unwrap();
        let s = Store::open(&d.path().join("upp.db")).unwrap();
        let (run, ids) = s.create_run(None, None, &specs()).unwrap();
        let parent = s.table(&ids[0]).unwrap().unwrap();
        let kids = s
            .add_tables(&run, &specs()[..1], None, Some(&parent))
            .unwrap();
        let kid = s.table(&kids[0]).unwrap().unwrap();
        assert_eq!(
            (kid.depth, kid.parent_id.as_deref()),
            (1, Some(ids[0].as_str()))
        );
        assert_eq!(s.children(&ids[0]).unwrap().len(), 1);
        // A dead owner's running table goes back to the queue, counted as a resume.
        s.claim("pid:999999:x", 10, Some(&run)).unwrap();
        assert_eq!(s.requeue("pid:999999:").unwrap(), 2);
        let t = s.table(&ids[0]).unwrap().unwrap();
        assert_eq!((t.status.as_str(), t.resumes), ("queued", 1));
        // Stopping the parent stops its sub-table too.
        s.request_stop(&ids[0]).unwrap();
        assert_eq!(s.table(&kids[0]).unwrap().unwrap().status, "stopped");
        // A waiter is woken by a write in this process.
        let mut w = s.watch();
        let db = d.path().join("upp.db");
        let h = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            Store::open(&db)
                .unwrap()
                .event("x", "ping", json!({}))
                .unwrap();
        });
        assert!(s.wait(&mut w, Duration::from_secs(5)));
        h.join().unwrap();
    }

    #[test]
    fn failed_upstream_skips_the_chain() {
        let d = tempfile::tempdir().unwrap();
        let s = Store::open(&d.path().join("upp.db")).unwrap();
        let (run, ids) = s.create_run(None, None, &specs()).unwrap();
        s.claim("me", 10, Some(&run)).unwrap();
        s.finish(&ids[0], "lost", None, "max rallies").unwrap();
        s.settle_queue().unwrap();
        assert_eq!(s.table(&ids[1]).unwrap().unwrap().status, "skipped");
        assert_eq!(s.table(&ids[2]).unwrap().unwrap().status, "skipped");
        assert_eq!(s.open_count(Some(&run)).unwrap(), 0);
    }

    #[test]
    fn stop_and_external_answers() {
        let d = tempfile::tempdir().unwrap();
        let s = Store::open(&d.path().join("upp.db")).unwrap();
        let (_run, ids) = s.create_run(None, None, &specs()).unwrap();
        s.request_stop(&ids[2]).unwrap();
        assert_eq!(s.table(&ids[2]).unwrap().unwrap().status, "stopped");
        assert!(s.answer(&ids[0], &json!({"verdict": "accept"})).is_err());
        s.push_pending(&ids[0], 1, &json!({"answer": "a"})).unwrap();
        s.set_status(&ids[0], "awaiting_server").unwrap();
        assert_eq!(s.pending(None).unwrap().len(), 1);
        assert_eq!(s.answer(&ids[0], &json!({"verdict": "accept"})).unwrap(), 1);
        assert_eq!(
            s.take_answer(&ids[0], 1).unwrap(),
            Some(json!({"verdict": "accept"}))
        );
        assert!(s.pending(None).unwrap().is_empty());
        // Resolve by name.
        assert_eq!(s.resolve("a").unwrap().id, ids[0]);
    }
}

// SPDX-License-Identifier: Apache-2.0
//! Model Context Protocol server over stdio (newline-delimited JSON-RPC).
//!
//! Tools: table_create, table_status, table_list, ball_answer, table_stop, table_wait,
//! cards_wait, card_answer, transcript, integrate.
//! An agent (Claude Code, Codex, Gemini CLI, opencode, OpenUnum) uses them to open
//! tables for workers and, with `server.mode = "external"`, to be the persistent
//! orchestrator itself: `cards_wait` blocks until any table's result card needs an
//! answer, `card_answer` answers it (accept / return / stop, chain `next`, `open`),
//! `transcript` fetches a rally's full text only when a card is not enough.
//! A player opens sub-tables with `table_create` and `parent` = its `UPP_TABLE`.

use serde_json::{json, Value};
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;
use upp_core::store::Store;
use upp_core::{api, MatchFile, Scheduler};

const PROTOCOL: &str = "2025-06-18";

fn tools() -> Value {
    json!([
        {
            "name": "table_create",
            "description": "Open one or more ping-pong tables. A table gives a ball (brief) to a player (worker: shell, openai, openunum, claude, codex, gemini, opencode), checks each answer with an external referee (command / file_exists / http_ok / json_path_equals / judge) and returns the ball with the referee output until a point is scored or the lease ends. Pass `match` as a JSON object ({defaults, table:[...]}, or a single table with a name), a TOML/JSON string, or `file` (path to a match file). Use server.mode=\"external\" to review every ball yourself with ball_answer. Returns run_id and table ids.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "match": {"description": "Match: JSON object or TOML/JSON text. Single-table shorthand: {name, goal, ball, cwd, player:{kind,...}, referee:[{kind,...}], lease:{wall_secs,max_rallies,max_tokens,max_cost_usd}, server:{mode}}."},
                    "file": {"type": "string", "description": "Path to a .toml/.json match file (relative cwd values resolve against its directory)."},
                    "parent": {"type": "string", "description": "Open the tables as sub-tables of this running table (a player passes its own UPP_TABLE). They inherit its player and working directory; the parent's rally waits for them before its referee runs. Depth is capped (max_depth, default 3)."}
                }
            }
        },
        {
            "name": "cards_wait",
            "description": "The orchestrator's inbox. Blocks until a result card arrives and returns compact cards (done claim, summary, evidence, open issues, artifacts, tokens, elapsed, referee pass/fail with an output tail, provenance), never transcripts. Without `since`: returns at once with every card that needs your answer now (needs_answer: true), else waits for the next card. With `since` (the `cursor` you got): returns every newer card, including final cards of ended tables. Answer needs_answer cards with card_answer; fetch details with transcript only when the card is not enough.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "tables": {"type": "array", "items": {"type": "string"}, "description": "Table ids or names (default: all)."},
                    "run": {"type": "string", "description": "Only this run."},
                    "since": {"type": "integer", "description": "Card cursor from the previous call."},
                    "timeout_secs": {"type": "integer", "description": "Default 60, max 600."}
                }
            },
            "annotations": {"readOnlyHint": true}
        },
        {
            "name": "card_answer",
            "description": "Answer a card that needs your answer. verdict: accept (scores only if the referee passed), return (the note goes back to the player with the referee output: write a precise correction), stop. Optional `next`: the next ball, chained after this table's point (a new table that starts when this one scores, with its result as {{prev.answer}}). Optional `open`: a new table object ({name, ball, goal, referee, ...}; player and cwd default to this table's) in the same run.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "table": {"type": "string"},
                    "verdict": {"type": "string", "enum": ["accept", "return", "stop"]},
                    "note": {"type": "string"},
                    "next": {"type": "string"},
                    "open": {"type": "object"}
                },
                "required": ["table", "verdict"]
            }
        },
        {
            "name": "transcript",
            "description": "The full ball, answer, referee output and provenance of one rally (or every rally) of a table. Use it only when a card is not enough: it is the expensive read.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "table": {"type": "string"},
                    "rally": {"type": "integer"}
                },
                "required": ["table"]
            },
            "annotations": {"readOnlyHint": true}
        },
        {
            "name": "integrate",
            "description": "Merge the verified points of a run's worktree tables (worktree = {...}) into one new branch upp/integrate-<run>, in table order; each table contributes exactly the tree its referee passed. Stops at the first conflict. `check` runs a command on the result.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "run": {"type": "string"},
                    "into": {"type": "string", "description": "Commit-ish to start from (default HEAD)."},
                    "check": {"type": "string"}
                },
                "required": ["run"]
            }
        },
        {
            "name": "table_status",
            "description": "One table (id or name): status, phase, rallies with referee checks and server notes, current ball, result, and the ball waiting for its external server (pending), if any.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "table": {"type": "string"},
                    "full": {"type": "boolean", "description": "Do not truncate balls and answers."}
                },
                "required": ["table"]
            },
            "annotations": {"readOnlyHint": true}
        },
        {
            "name": "table_list",
            "description": "List tables (newest first) with status, rallies, trail (. returned, ! fault, * point), elapsed time and tokens; plus the count of balls waiting for an external server.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "active_only": {"type": "boolean", "description": "Hide ended tables."},
                    "run": {"type": "string", "description": "Only this run id."}
                }
            },
            "annotations": {"readOnlyHint": true}
        },
        {
            "name": "ball_answer",
            "description": "Answer the ball a table is waiting on (server.mode=\"external\"). verdict: accept (scores the point, only if the referee passed), return (sends `note` back to the player with the referee output), stop (ends the table). Write the note as a precise correction or the next step.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "table": {"type": "string"},
                    "verdict": {"type": "string", "enum": ["accept", "return", "stop"]},
                    "note": {"type": "string"}
                },
                "required": ["table", "verdict"]
            }
        },
        {
            "name": "table_stop",
            "description": "Stop a table before its next rally (a queued table stops at once).",
            "inputSchema": {
                "type": "object",
                "properties": {"table": {"type": "string"}},
                "required": ["table"]
            },
            "annotations": {"destructiveHint": true}
        },
        {
            "name": "table_wait",
            "description": "Block until one of the given tables (or any active table) waits for its external server, or all of them ended, or timeout_secs passes (default 60, max 600). Returns the pending balls and table summaries. Use it instead of polling.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "tables": {"type": "array", "items": {"type": "string"}},
                    "timeout_secs": {"type": "integer"}
                }
            },
            "annotations": {"readOnlyHint": true}
        }
    ])
}

fn call_tool(db: &Path, name: &str, a: &Value) -> Result<Value, String> {
    let s = |k: &str| a.get(k).and_then(|v| v.as_str()).map(String::from);
    let store = Store::open(db)?;
    match name {
        "table_create" if s("parent").is_some() => {
            let parent = s("parent").unwrap_or_default();
            let m = match s("file") {
                Some(f) => Value::String(
                    std::fs::read_to_string(&f).map_err(|e| format!("cannot read {f}: {e}"))?,
                ),
                None => a.get("match").cloned().ok_or("give `match` or `file`")?,
            };
            api::create_child(&store, &parent, m)
        }
        "table_create" => {
            if let Some(f) = s("file") {
                let m = MatchFile::load(Path::new(&f))?;
                api::create(&store, m, Some(&f))
            } else {
                let m = a.get("match").cloned().ok_or("give `match` or `file`")?;
                api::create_json(&store, m)
            }
        }
        "table_status" => api::status(
            &store,
            &s("table").ok_or("table is required")?,
            a.get("full").and_then(|v| v.as_bool()).unwrap_or(false),
        ),
        "table_list" => api::list(
            &store,
            a.get("active_only")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            s("run").as_deref(),
        ),
        "ball_answer" => api::answer(
            &store,
            &s("table").ok_or("table is required")?,
            &s("verdict").ok_or("verdict is required")?,
            &s("note").unwrap_or_default(),
        ),
        "table_stop" => api::stop(&store, &s("table").ok_or("table is required")?),
        "table_wait" => {
            let ids: Vec<String> = a
                .get("tables")
                .and_then(|v| v.as_array())
                .map(|v| {
                    v.iter()
                        .filter_map(|x| x.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            let t = a
                .get("timeout_secs")
                .and_then(|v| v.as_u64())
                .unwrap_or(60)
                .min(600);
            api::wait(db, &ids, Duration::from_secs(t))
        }
        "cards_wait" => {
            let q = api::CardQuery {
                tables: a
                    .get("tables")
                    .and_then(|v| v.as_array())
                    .map(|v| {
                        v.iter()
                            .filter_map(|x| x.as_str().map(String::from))
                            .collect()
                    })
                    .unwrap_or_default(),
                run: s("run"),
                since: a.get("since").and_then(|v| v.as_i64()),
                timeout: Duration::from_secs(
                    a.get("timeout_secs")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(60)
                        .min(600),
                ),
            };
            api::cards_wait(db, &q)
        }
        "card_answer" => api::card_answer(
            &store,
            &s("table").ok_or("table is required")?,
            &s("verdict").ok_or("verdict is required")?,
            &s("note").unwrap_or_default(),
            s("next").as_deref(),
            a.get("open").filter(|o| o.is_object()),
        ),
        "transcript" => api::transcript(
            &store,
            &s("table").ok_or("table is required")?,
            a.get("rally").and_then(|v| v.as_u64()).map(|n| n as u32),
        ),
        "integrate" => api::integrate(
            &store,
            &s("run").ok_or("run is required")?,
            s("into").as_deref(),
            s("check").as_deref(),
        ),
        _ => Err(format!("unknown tool '{name}'")),
    }
}

pub fn handle(db: &Path, msg: &Value) -> Option<Value> {
    let id = msg.get("id").cloned()?;
    let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("");
    let params = msg.get("params").cloned().unwrap_or(Value::Null);
    let result = match method {
        "initialize" => {
            let v = params
                .get("protocolVersion")
                .and_then(|v| v.as_str())
                .unwrap_or(PROTOCOL);
            json!({
                "protocolVersion": v,
                "capabilities": {"tools": {"listChanged": false}},
                "serverInfo": {"name": "unumpingpong", "version": upp_core::VERSION}
            })
        }
        "ping" => json!({}),
        "tools/list" => json!({"tools": tools()}),
        "tools/call" => {
            let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let args = params.get("arguments").cloned().unwrap_or(json!({}));
            match call_tool(db, name, &args) {
                Ok(v) => {
                    json!({"content": [{"type": "text", "text": v.to_string()}], "structuredContent": v, "isError": false})
                }
                Err(e) => json!({"content": [{"type": "text", "text": e}], "isError": true}),
            }
        }
        _ => {
            return Some(
                json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": format!("method not found: {method}")}}),
            )
        }
    };
    Some(json!({"jsonrpc": "2.0", "id": id, "result": result}))
}

pub fn run(db: &Path, parallel: usize, queue_only: bool) -> Result<(), String> {
    let db: PathBuf = std::path::absolute(db).unwrap_or_else(|_| db.to_path_buf());
    Store::open(&db)?;
    if !queue_only {
        let sched = Scheduler::new(&db, parallel, "mcp");
        std::thread::spawn(move || {
            if let Err(e) = sched.run() {
                eprintln!("upp mcp: scheduler stopped: {e}");
            }
        });
    }
    let stdin = std::io::stdin();
    let mut out = std::io::stdout();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let reply = match serde_json::from_str::<Value>(&line) {
            Ok(msg) => handle(&db, &msg),
            Err(e) => Some(
                json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32700, "message": e.to_string()}}),
            ),
        };
        if let Some(r) = reply {
            let _ = writeln!(out, "{r}");
            let _ = out.flush();
        }
    }
    // stdin closed: the client is gone. Mark this process's running tables.
    if let Ok(s) = Store::open(&db) {
        let _ = s.orphan(&upp_core::scheduler::owner_tag("mcp"));
    }
    Ok(())
}

// SPDX-License-Identifier: Apache-2.0
//! The persistent orchestrator: one long-lived player (any adapter) that receives the
//! cards of many tables and answers each at once: accept, return with a correction,
//! chain the next ball, open a new table, or stop. It never waits on one table: it
//! sleeps until *any* card in its scope needs an answer (event-driven), sends every
//! waiting card in one message (up to `batch`), and applies the decisions.
//!
//! It reads cards and referee output only; transcripts stay in the store. The referee
//! still decides whether a point is possible, and if the orchestrator fails or skips a
//! card, the card gets the referee's default (pass: accept, fail: return) so work never
//! stalls on it.

use crate::api;
use crate::config::OrchestratorSpec;
use crate::player::{self, PlayCtx, Player};
use crate::proc;
use crate::referee::extract_json;
use crate::runner::card_text;
use crate::store::{PendingBall, Store};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

pub const SYSTEM: &str = "You are the orchestrator (server) of UnumPingPong tables. Workers \
(players) answer balls; an external referee checks every answer with deterministic evidence. \
You receive compact result cards, never transcripts. For EACH card decide at once:\n\
- accept: the work is done (scores only if the referee passed);\n\
- return: send a precise correction (at most 6 lines, actionable; do not solve the task yourself);\n\
- stop: end the table.\n\
Optionally chain the next ball after an accept (\"next\": the next task, it starts when this \
table scores and gets its result), or open a new table (\"open\": {\"name\", \"ball\", \"goal\"}).\n\
Reply with JSON only: {\"decisions\": [{\"table\": \"<table id>\", \"verdict\": \
\"accept|return|stop\", \"note\": \"...\", \"next\": \"...\", \"open\": {...}}]}";

pub struct Orchestrator {
    pub db: PathBuf,
    pub spec: OrchestratorSpec,
    /// Serve only this run's tables; `None`: every table waiting for an external server.
    pub run: Option<String>,
    /// Return when the run has no open table left.
    pub until_idle: bool,
    pub stop: Arc<AtomicBool>,
}

/// One decision for one card.
#[derive(Debug, Clone, PartialEq)]
pub struct Decision {
    pub table: String,
    pub verdict: String,
    pub note: String,
    pub next: Option<String>,
    pub open: Option<Value>,
}

/// Parse the orchestrator's reply: `{"decisions": [...]}`, a bare array, or one object.
pub fn parse_decisions(text: &str) -> Vec<Decision> {
    let v = match extract_json(text) {
        Some(v) => v,
        None => return vec![],
    };
    let list = match &v {
        Value::Array(a) => a.clone(),
        Value::Object(o) if o.contains_key("decisions") => {
            v["decisions"].as_array().cloned().unwrap_or_default()
        }
        Value::Object(_) => vec![v.clone()],
        _ => vec![],
    };
    list.iter()
        .filter_map(|d| {
            let table = d["table"].as_str().or(d["table_id"].as_str())?.to_string();
            let verdict = d["verdict"].as_str().unwrap_or("").to_ascii_lowercase();
            if !matches!(verdict.as_str(), "accept" | "return" | "stop") {
                return None;
            }
            Some(Decision {
                table,
                verdict,
                note: d["note"].as_str().unwrap_or("").to_string(),
                next: d["next"]
                    .as_str()
                    .filter(|s| !s.trim().is_empty())
                    .map(String::from),
                open: d.get("open").filter(|o| o.is_object()).cloned(),
            })
        })
        .collect()
}

/// The message for a batch of cards.
pub fn cards_message(batch: &[PendingBall]) -> String {
    let mut s = String::new();
    for p in batch {
        let c = &p.payload["card"];
        s.push_str(&format!(
            "CARD {} {} rally={} referee={} done={}\n{}\ngoal: {}\n\n",
            p.table_id,
            c["table"].as_str().unwrap_or("?").replace(' ', "_"),
            p.rally,
            if p.payload["referee"]["passed"] == true {
                "PASS"
            } else {
                "FAIL"
            },
            if c["done"] == true { "yes" } else { "no" },
            card_text(c),
            proc::head(p.payload["goal"].as_str().unwrap_or(""), 400),
        ));
    }
    s.push_str(&format!(
        "{} card(s). Decide each one now (JSON only).",
        batch.len()
    ));
    s
}

impl Orchestrator {
    pub fn new(db: &Path, spec: OrchestratorSpec) -> Orchestrator {
        Orchestrator {
            db: db.to_path_buf(),
            spec,
            run: None,
            until_idle: false,
            stop: Arc::new(AtomicBool::new(false)),
        }
    }

    fn fresh_player(&self, ctx: &PlayCtx) -> Result<Box<dyn Player>, String> {
        player::build(&self.spec.player, ctx)
    }

    /// Serve cards until stopped (or idle, with `until_idle`). Returns counters.
    pub fn run(&self) -> Result<Value, String> {
        let store = Store::open(&self.db)?;
        let batch_max = self.spec.batch.unwrap_or(8).max(1);
        let refresh = self.spec.refresh_every.unwrap_or(40).max(1);
        let max_new = self.spec.max_new_tables.unwrap_or(50);
        let key = format!(
            "orchestrator-{}",
            &uuid::Uuid::new_v4().simple().to_string()[..8]
        );
        let far = Instant::now() + Duration::from_secs(10 * 365 * 24 * 3600);
        let cwd = PathBuf::from(self.spec.cwd.clone().unwrap_or_else(|| ".".into()));
        let mut ctx = PlayCtx::new(&key, 0, cwd, far, &key);
        ctx.env.insert(
            "UPP_DB".into(),
            std::path::absolute(&self.db)
                .unwrap_or(self.db.clone())
                .to_string_lossy()
                .into_owned(),
        );
        let mut player = self.fresh_player(&ctx)?;
        let mut fresh = true;
        let (mut handled, mut since_refresh, mut opened, mut calls, mut fallbacks) =
            (0u64, 0u32, 0u32, 0u64, 0u64);
        let mut w = store.watch();
        loop {
            if self.stop.load(Ordering::SeqCst) {
                break;
            }
            let mut pending: Vec<PendingBall> = Vec::new();
            for p in store.pending(None)? {
                let ok = match &self.run {
                    Some(r) => store.table(&p.table_id)?.is_some_and(|t| &t.run_id == r),
                    None => true,
                };
                if ok {
                    pending.push(p);
                }
            }
            if pending.is_empty() {
                if self.until_idle && store.open_count(self.run.as_deref())? == 0 {
                    break;
                }
                store.wait(&mut w, Duration::from_secs(5));
                continue;
            }
            let batch: Vec<PendingBall> = pending.into_iter().take(batch_max).collect();
            let mut msg = String::new();
            if fresh {
                msg.push_str(SYSTEM);
                if let Some(i) = &self.spec.instructions {
                    msg.push_str(&format!("\n\nStanding instructions:\n{}", i.trim()));
                }
                msg.push_str("\n\n");
            }
            msg.push_str(&cards_message(&batch));
            ctx.rally += 1;
            calls += 1;
            let decisions = match player.play(&msg, &ctx) {
                Ok(r) => {
                    let share = (r.tokens_in + r.tokens_out) / batch.len() as u64;
                    for p in &batch {
                        store.add_server_tokens(&p.table_id, share)?;
                    }
                    fresh = false;
                    parse_decisions(&r.text)
                }
                Err(e) => {
                    for p in &batch {
                        store.event(
                            &p.table_id,
                            "orchestrator_error",
                            json!({"error": proc::head(&e, 300)}),
                        )?;
                    }
                    player = self.fresh_player(&ctx)?;
                    fresh = true;
                    vec![]
                }
            };
            for p in &batch {
                let name = p.payload["card"]["table"].as_str().unwrap_or("");
                let d = decisions
                    .iter()
                    .find(|d| d.table == p.table_id || (!name.is_empty() && d.table == name))
                    .cloned();
                let d = match d {
                    Some(d) => d,
                    None => {
                        fallbacks += 1;
                        Decision {
                            table: p.table_id.clone(),
                            verdict: if p.payload["referee"]["passed"] == true {
                                "accept".into()
                            } else {
                                "return".into()
                            },
                            note: String::new(),
                            next: None,
                            open: None,
                        }
                    }
                };
                let allow_new = opened < max_new;
                let next = d.next.as_deref().filter(|_| allow_new);
                let open = d.open.as_ref().filter(|_| allow_new);
                match api::card_answer(&store, &p.table_id, &d.verdict, &d.note, next, open) {
                    Ok(r) => {
                        opened += r["created"].as_array().map(|a| a.len()).unwrap_or(0) as u32;
                        store.event(
                            &p.table_id,
                            "orchestrator",
                            json!({"verdict": d.verdict, "note": proc::head(&d.note, 300), "created": r["created"]}),
                        )?;
                    }
                    Err(e) => store.event(
                        &p.table_id,
                        "orchestrator_error",
                        json!({"error": proc::head(&e, 300)}),
                    )?,
                }
                handled += 1;
                since_refresh += 1;
            }
            if since_refresh >= refresh {
                // A fresh session keeps the orchestrator's own context small.
                player = self.fresh_player(&ctx)?;
                fresh = true;
                since_refresh = 0;
            }
        }
        Ok(json!({"cards": handled, "calls": calls, "fallbacks": fallbacks, "opened": opened}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decisions_parse_in_every_shape() {
        let d = parse_decisions(
            "Sure.\n```json\n{\"decisions\": [{\"table\": \"t1\", \"verdict\": \"ACCEPT\", \"next\": \"write docs\"},\
             {\"table\": \"t2\", \"verdict\": \"return\", \"note\": \"handle 0\"}, {\"table\": \"t3\", \"verdict\": \"maybe\"}]}\n```",
        );
        assert_eq!(d.len(), 2);
        assert_eq!(d[0].verdict, "accept");
        assert_eq!(d[0].next.as_deref(), Some("write docs"));
        assert_eq!(d[1].note, "handle 0");
        let d = parse_decisions("[{\"table_id\": \"x\", \"verdict\": \"stop\"}]");
        assert_eq!(d[0].table, "x");
        let d = parse_decisions(
            "{\"table\": \"y\", \"verdict\": \"accept\", \"open\": {\"name\": \"n\", \"ball\": \"b\"}}",
        );
        assert!(d[0].open.is_some());
        assert!(parse_decisions("no json here").is_empty());
    }

    #[test]
    fn the_message_carries_cards_not_transcripts() {
        let p = PendingBall {
            table_id: "t1".into(),
            rally: 2,
            payload: json!({"goal": "g", "referee": {"passed": false},
                "card": {"table": "fizz buzz", "done": true, "summary": "S", "evidence": ["E"],
                         "referee": {"passed": false, "failed": ["unittest: exit 1"], "tail": "AssertionError"}},
                "answer": "THE FULL ANSWER"}),
            created_ms: 0,
        };
        let m = cards_message(&[p]);
        assert!(
            m.starts_with("CARD t1 fizz_buzz rally=2 referee=FAIL done=yes\n"),
            "{m}"
        );
        assert!(m.contains("AssertionError") && m.contains("summary: S"));
        assert!(!m.contains("THE FULL ANSWER"));
    }
}

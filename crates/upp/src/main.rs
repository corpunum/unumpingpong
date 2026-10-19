// SPDX-License-Identifier: Apache-2.0
//! `upp`: the UnumPingPong CLI.

mod mcp;
mod tui;

use serde_json::{json, Value};
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::time::Duration;
use upp_core::store::{default_db_path, Store, TableRow};
use upp_core::{api, MatchFile, Parallel, Scheduler};

const HELP: &str = "upp: orchestrator <-> worker ping-pong tables with an external referee

USAGE:
  upp [--db PATH] <command> [args]

COMMANDS:
  run <match.toml|json>     Run a match in the foreground (exit 0 when every table scores)
      --parallel N|auto       Max tables at once (default: defaults.max_parallel, else 4);
                              auto probes the OpenAI-compatible endpoints for their slots
      --watch                 Live TUI while it runs
      --exit-when-done        With --watch: leave the TUI when every table has ended
      --json                  Print the final summary as JSON
                              A match with [orchestrator] runs its persistent orchestrator too.
  create <match> [--parent ID]
                            Queue a match for a running daemon (uppd) or `upp mcp`;
                            --parent opens sub-tables of a running table (nested)
  validate <match>          Parse and check a match file; print the resolved tables
  watch [--run ID] [--all]  Live TUI of all tables (reads the store; works with any runner)
      --exit-when-done        Leave when every shown table has ended
  list [--all] [--json]     Tables of the latest run (--all: every run)
  status <table> [--full]   One table: rallies as result cards, token split (JSON)
  transcript <table> [--rally N]
                            Full balls, answers and referee output (on demand)
  cards [--run ID] [--table T]... [--since N] [--wait SECS]
                            Result cards (JSON); --wait blocks until one arrives
  pending                   Balls waiting for an external server (JSON)
  answer <table> <accept|return|stop> [note...] [--next BALL]
                            Answer a ball as the external server; --next chains a table
  orchestrate <file> [--run ID]
                            Run the file's [orchestrator] as a long-lived server for every
                            waiting card (of one run, or all runs)
  stop <table> | --run ID   Stop a table (its running player call is killed) or a whole run
  resume [--run ID]         Run tables requeued after their scheduler died (crash recovery)
  integrate [--run ID] [--into REF] [--check CMD]
                            Merge the verified points of worktree tables into one branch
  export [--run ID] [-o F]  Every rally as JSON lines (trace / training data)
  mcp [--parallel N] [--queue-only]
                            MCP server on stdio (runs queued tables unless --queue-only)

GLOBAL:
  --db PATH                 Store (default $UPP_DB, else ./.upp/upp.db)
  -V, --version             Print the version

Docs: https://github.com/corpunum/unumpingpong";

fn die(msg: &str) -> ! {
    eprintln!("upp: {msg}");
    std::process::exit(2)
}

fn main() {
    if let Err(e) = real_main() {
        die(&e);
    }
}

#[derive(Default)]
struct Args {
    db: Option<PathBuf>,
    cmd: Option<String>,
    pos: Vec<String>,
    parallel: Option<String>,
    rally: Option<u32>,
    since: Option<i64>,
    wait: Option<u64>,
    next: Option<String>,
    parent: Option<String>,
    into: Option<String>,
    check: Option<String>,
    tables: Vec<String>,
    watch: bool,
    exit_when_done: bool,
    json: bool,
    all: bool,
    full: bool,
    queue_only: bool,
    run: Option<String>,
    out: Option<PathBuf>,
}

fn parse_args() -> Result<Args, String> {
    use lexopt::prelude::*;
    let mut a = Args::default();
    let mut p = lexopt::Parser::from_env();
    while let Some(arg) = p.next().map_err(|e| e.to_string())? {
        match arg {
            Long("db") => a.db = Some(p.value().map_err(|e| e.to_string())?.into()),
            Long("parallel") | Short('j') => {
                a.parallel = Some(
                    p.value()
                        .map_err(|e| e.to_string())?
                        .string()
                        .map_err(|_| "bad --parallel")?,
                )
            }
            Long("rally") => {
                a.rally = Some(
                    p.value()
                        .map_err(|e| e.to_string())?
                        .parse()
                        .map_err(|e| format!("--rally: {e}"))?,
                )
            }
            Long("since") => {
                a.since = Some(
                    p.value()
                        .map_err(|e| e.to_string())?
                        .parse()
                        .map_err(|e| format!("--since: {e}"))?,
                )
            }
            Long("wait") => {
                a.wait = Some(
                    p.value()
                        .map_err(|e| e.to_string())?
                        .parse()
                        .map_err(|e| format!("--wait: {e}"))?,
                )
            }
            Long(k @ ("next" | "parent" | "into" | "check" | "table")) => {
                let k = k.to_string();
                let v = p
                    .value()
                    .map_err(|e| e.to_string())?
                    .string()
                    .map_err(|_| "non-UTF-8 value")?;
                match k.as_str() {
                    "next" => a.next = Some(v),
                    "parent" => a.parent = Some(v),
                    "into" => a.into = Some(v),
                    "check" => a.check = Some(v),
                    _ => a.tables.push(v),
                }
            }
            Long("watch") | Short('w') => a.watch = true,
            Long("exit-when-done") => a.exit_when_done = true,
            Long("json") => a.json = true,
            Long("all") | Short('a') => a.all = true,
            Long("full") => a.full = true,
            Long("queue-only") => a.queue_only = true,
            Long("run") => {
                a.run = Some(
                    p.value()
                        .map_err(|e| e.to_string())?
                        .string()
                        .map_err(|_| "bad --run")?,
                )
            }
            Short('o') | Long("output") => {
                a.out = Some(p.value().map_err(|e| e.to_string())?.into())
            }
            Short('V') | Long("version") => {
                println!("upp {}", upp_core::VERSION);
                std::process::exit(0);
            }
            Short('h') | Long("help") => {
                println!("{HELP}");
                std::process::exit(0);
            }
            Value(v) => {
                let s = v.string().map_err(|_| "non-UTF-8 argument")?;
                if a.cmd.is_none() {
                    a.cmd = Some(s);
                } else {
                    a.pos.push(s);
                }
            }
            _ => return Err(arg.unexpected().to_string()),
        }
    }
    Ok(a)
}

fn print_json(v: &Value) {
    println!("{}", serde_json::to_string_pretty(v).unwrap_or_default());
}

fn real_main() -> Result<(), String> {
    let a = parse_args()?;
    let db = a.db.clone().unwrap_or_else(default_db_path);
    let Some(cmd) = a.cmd.clone() else {
        println!("{HELP}");
        return Ok(());
    };
    let need = |i: usize, what: &str| -> Result<String, String> {
        a.pos
            .get(i)
            .cloned()
            .ok_or(format!("{cmd}: missing {what}"))
    };
    match cmd.as_str() {
        "run" => cmd_run(&db, Path::new(&need(0, "match file")?), &a),
        "create" => {
            let path = need(0, "match file")?;
            let store = Store::open(&db)?;
            match &a.parent {
                Some(parent) => {
                    let text = std::fs::read_to_string(&path)
                        .map_err(|e| format!("cannot read {path}: {e}"))?;
                    print_json(&api::create_child(&store, parent, Value::String(text))?);
                }
                None => {
                    let m = MatchFile::load(Path::new(&path))?;
                    print_json(&api::create(&store, m, Some(&path))?);
                }
            }
            Ok(())
        }
        "transcript" => {
            let store = Store::open(&db)?;
            print_json(&api::transcript(&store, &need(0, "table")?, a.rally)?);
            Ok(())
        }
        "cards" => {
            let q = api::CardQuery {
                tables: a.tables.clone(),
                run: a.run.clone(),
                since: a.since.or(if a.wait.is_some() { None } else { Some(0) }),
                timeout: Duration::from_secs(a.wait.unwrap_or(0)),
            };
            print_json(&api::cards_wait(&db, &q)?);
            Ok(())
        }
        "orchestrate" => {
            let m = MatchFile::load(Path::new(&need(0, "file with [orchestrator]")?))?;
            let spec = m
                .orchestrator
                .ok_or("the file has no [orchestrator] section")?;
            let mut o = upp_core::orchestrator::Orchestrator::new(&db, spec);
            o.run = a.run.clone();
            let stop = o.stop.clone();
            let _ = ctrlc::set_handler(move || stop.store(true, Ordering::SeqCst));
            eprintln!(
                "upp: orchestrating {} (Ctrl-C to stop)",
                a.run.as_deref().unwrap_or("every run")
            );
            print_json(&o.run()?);
            Ok(())
        }
        "resume" => {
            let store = Store::open(&db)?;
            let n = upp_core::scheduler::reap_dead_owners(&store)?;
            let parallel = match a.parallel.as_deref() {
                Some(p) => p.parse().map_err(|e| format!("--parallel: {e}"))?,
                None => 4,
            };
            let mut sched = Scheduler::new(&db, parallel, "resume");
            sched.run = a.run.clone();
            sched.until_idle = true;
            eprintln!("upp: {n} table(s) requeued; running the queue");
            sched.run()?;
            print_json(
                &json!({"requeued": n, "tables": store.tables(a.run.as_deref(), false)?.iter().take(50).map(api::summary).collect::<Vec<_>>()}),
            );
            Ok(())
        }
        "integrate" => {
            let store = Store::open(&db)?;
            let run = match &a.run {
                Some(r) => r.clone(),
                None => store.latest_run()?.ok_or("no runs")?,
            };
            let v = api::integrate(&store, &run, a.into.as_deref(), a.check.as_deref())?;
            print_json(&v);
            if v["ok"] != true {
                std::process::exit(1);
            }
            Ok(())
        }
        "validate" => {
            let m = MatchFile::load(Path::new(&need(0, "match file")?))?;
            let t = m.resolve()?;
            print_json(&json!({"ok": true, "tables": t}));
            Ok(())
        }
        "watch" => {
            if !std::io::stdout().is_terminal() {
                return Err("watch needs a terminal (use `upp list --json` in scripts)".into());
            }
            let store = Store::open(&db)?;
            let _ = upp_core::scheduler::reap_dead_owners(&store);
            let run = if a.all {
                None
            } else {
                a.run.clone().or(store.latest_run()?)
            };
            tui::watch(&db, run, a.exit_when_done, None)
        }
        "list" => {
            let store = Store::open(&db)?;
            let _ = upp_core::scheduler::reap_dead_owners(&store);
            let run = if a.all {
                None
            } else {
                a.run.clone().or(store.latest_run()?)
            };
            let rows = store.tables(run.as_deref(), false)?;
            if a.json {
                print_json(&json!({"tables": rows.iter().map(api::summary).collect::<Vec<_>>()}));
            } else {
                print_table_list(&rows);
            }
            Ok(())
        }
        "status" => {
            let store = Store::open(&db)?;
            print_json(&api::status(&store, &need(0, "table")?, a.full)?);
            Ok(())
        }
        "pending" => {
            let store = Store::open(&db)?;
            let p = store.pending(None)?;
            print_json(&json!({"pending": p}));
            Ok(())
        }
        "answer" => {
            let store = Store::open(&db)?;
            let note = a.pos.get(2..).map(|s| s.join(" ")).unwrap_or_default();
            print_json(&api::card_answer(
                &store,
                &need(0, "table")?,
                &need(1, "verdict")?,
                &note,
                a.next.as_deref(),
                None,
            )?);
            Ok(())
        }
        "stop" => {
            let store = Store::open(&db)?;
            if let Some(run) = &a.run {
                let mut n = 0;
                for t in store.tables(Some(run), true)? {
                    store.request_stop(&t.id)?;
                    n += 1;
                }
                print_json(&json!({"ok": true, "stopped": n}));
            } else {
                print_json(&api::stop(&store, &need(0, "table")?)?);
            }
            Ok(())
        }
        "export" => {
            let store = Store::open(&db)?;
            let s = api::export_jsonl(&store, a.run.as_deref())?;
            match &a.out {
                Some(p) => std::fs::write(p, s).map_err(|e| format!("{}: {e}", p.display())),
                None => {
                    print!("{s}");
                    Ok(())
                }
            }
        }
        "mcp" => {
            let p = match a.parallel.as_deref() {
                Some(p) => p.parse().map_err(|e| format!("--parallel: {e}"))?,
                None => 4,
            };
            mcp::run(&db, p, a.queue_only)
        }
        other => Err(format!("unknown command '{other}' (see upp --help)")),
    }
}

fn cmd_run(db: &Path, file: &Path, a: &Args) -> Result<(), String> {
    let m = MatchFile::load(file)?;
    let want = a
        .parallel
        .clone()
        .map(|p| match p.parse::<usize>() {
            Ok(n) => Parallel::N(n),
            Err(_) => Parallel::Word(p),
        })
        .or(m.defaults.max_parallel.clone());
    let parallel = match want {
        None => 4,
        Some(Parallel::N(n)) => n,
        Some(Parallel::Word(w)) if w == "auto" => {
            let (n, how) = upp_core::probe::auto_cap(&m.resolve()?);
            eprintln!("upp: --parallel auto -> {n} ({how})");
            n
        }
        Some(Parallel::Word(w)) => {
            return Err(format!("--parallel: expected a number or auto, got {w:?}"))
        }
    };
    let orchestrator = m.orchestrator.clone();
    let store = Store::open(db)?;
    let created = api::create(&store, m, Some(&file.to_string_lossy()))?;
    let run_id = created["run_id"].as_str().unwrap_or_default().to_string();
    let mut sched = Scheduler::new(db, parallel, "run");
    sched.run = Some(run_id.clone());
    sched.until_idle = true;
    let stop = sched.stop.clone();
    let owner = sched.owner.clone();
    let orch = orchestrator.map(|spec| {
        let mut o = upp_core::orchestrator::Orchestrator::new(db, spec);
        o.run = Some(run_id.clone());
        o.until_idle = true;
        let ostop = o.stop.clone();
        (std::thread::spawn(move || o.run()), ostop)
    });
    {
        let db = db.to_path_buf();
        let stop = stop.clone();
        let owner = owner.clone();
        let run_id = run_id.clone();
        let _ = ctrlc::set_handler(move || {
            stop.store(true, Ordering::SeqCst);
            // Real cancellation: a stop request kills each running player's process group.
            if let Ok(s) = Store::open(&db) {
                for t in s.tables(Some(&run_id), true).unwrap_or_default() {
                    let _ = s.request_stop(&t.id);
                }
                let until = std::time::Instant::now() + Duration::from_secs(3);
                while std::time::Instant::now() < until
                    && s.open_count(Some(&run_id)).unwrap_or(0) > 0
                {
                    std::thread::sleep(Duration::from_millis(100));
                }
                let _ = s.orphan(&owner);
            }
            eprintln!("\nupp: interrupted; running tables stopped (player processes killed)");
            std::process::exit(130);
        });
    }
    let handle = std::thread::spawn(move || sched.run());
    if a.watch && std::io::stdout().is_terminal() {
        tui::watch(db, Some(run_id.clone()), a.exit_when_done, Some(&handle))?;
        if !handle.is_finished() {
            // The viewer left early: stop claiming and mark what is still running.
            stop.store(true, Ordering::SeqCst);
            for t in store.tables(Some(&run_id), true)? {
                store.request_stop(&t.id)?;
            }
            eprintln!("upp: waiting for running rallies to end (Ctrl-C to abandon)...");
        }
    } else if !a.json {
        tail_events(db, &run_id, &handle)?;
    }
    handle
        .join()
        .map_err(|_| "scheduler panicked".to_string())??;
    if let Some((h, ostop)) = orch {
        ostop.store(true, Ordering::SeqCst);
        upp_core::notify::bump();
        match h.join() {
            Ok(Ok(stats)) if !a.json => eprintln!("upp: orchestrator {stats}"),
            Ok(Err(e)) => eprintln!("upp: orchestrator stopped: {e}"),
            _ => {}
        }
    }
    let rows = store.tables(Some(&run_id), false)?;
    if a.json {
        print_json(
            &json!({"run_id": run_id, "tables": rows.iter().map(api::summary).collect::<Vec<_>>()}),
        );
    } else {
        println!();
        print_table_list(&rows);
    }
    let points = rows.iter().filter(|t| t.status == "point").count();
    if points == rows.len() {
        Ok(())
    } else {
        std::process::exit(1)
    }
}

/// Print one line per event of this run until the scheduler ends.
fn tail_events(
    db: &Path,
    run_id: &str,
    handle: &std::thread::JoinHandle<Result<(), String>>,
) -> Result<(), String> {
    let store = Store::open(db)?;
    let names: std::collections::HashMap<String, String> = store
        .tables(Some(run_id), false)?
        .into_iter()
        .map(|t| (t.id, t.name))
        .collect();
    let width = names.values().map(|n| n.len()).max().unwrap_or(4).min(24);
    let mut last = 0i64;
    loop {
        let done = handle.is_finished();
        for ev in store.events_since(last, 500)? {
            last = ev.id;
            let Some(name) = names.get(&ev.table_id) else {
                continue;
            };
            let d = &ev.data;
            let line = match ev.kind.as_str() {
                "started" => "start".to_string(),
                "rally" => format!("rally {} -> player", d["n"]),
                "answer" => match d["tokens"].as_u64() {
                    Some(t) if t > 0 => format!("rally {} <- answer ({t} tokens)", d["n"]),
                    _ => format!("rally {} <- answer", d["n"]),
                },
                "fault" => format!(
                    "rally {} FAULT: {}",
                    d["n"],
                    one_line(d["error"].as_str().unwrap_or(""))
                ),
                "referee" => {
                    let checks: Vec<String> = d["checks"]
                        .as_array()
                        .map(|a| {
                            a.iter()
                                .map(|c| {
                                    format!(
                                        "{} {}",
                                        if c["pass"].as_bool() == Some(true) {
                                            "ok"
                                        } else {
                                            "FAIL"
                                        },
                                        one_line(c["name"].as_str().unwrap_or(""))
                                    )
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    format!(
                        "rally {} referee {}: {}",
                        d["n"],
                        if d["passed"].as_bool() == Some(true) {
                            "PASS"
                        } else {
                            "FAIL"
                        },
                        checks.join("; ")
                    )
                }
                "awaiting_server" => format!("rally {} waiting for the external server", d["n"]),
                "return" => format!("rally {} ball returned", d["n"]),
                "orchestrator" => format!(
                    "orchestrator: {} {}",
                    d["verdict"].as_str().unwrap_or(""),
                    one_line(d["note"].as_str().unwrap_or(""))
                ),
                "children" => format!("opened sub-tables {}", d["tables"]),
                "waiting_children" => "waiting for its sub-tables".to_string(),
                "resumed" => format!("resumed after rally {}", d["after_rally"]),
                "committed" => format!(
                    "verified tree committed {}",
                    d["commit"]
                        .as_str()
                        .unwrap_or("")
                        .chars()
                        .take(10)
                        .collect::<String>()
                ),
                "point" | "lost" | "expired" | "stopped" | "error" | "skipped" => format!(
                    "{} ({})",
                    ev.kind.to_uppercase(),
                    d["reason"].as_str().unwrap_or("")
                ),
                _ => continue,
            };
            println!("{}  {:<width$}  {}", clock(ev.ts), name, line);
        }
        if done {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(150));
    }
}

fn one_line(s: &str) -> String {
    let l = s.lines().next().unwrap_or("");
    if l.chars().count() > 80 {
        format!("{}...", l.chars().take(80).collect::<String>())
    } else {
        l.to_string()
    }
}

fn clock(ms: i64) -> String {
    let s = (ms / 1000) % 86400;
    format!("{:02}:{:02}:{:02}Z", s / 3600, (s / 60) % 60, s % 60)
}

fn print_table_list(rows: &[TableRow]) {
    if rows.is_empty() {
        println!("no tables");
        return;
    }
    let now = upp_core::store::now_ms();
    let w = rows
        .iter()
        .map(|t| t.name.len())
        .max()
        .unwrap_or(4)
        .clamp(4, 28);
    println!(
        "{:<12} {:<w$} {:<16} {:<15} {:>7} {:<8} {:>7} {:>9}",
        "ID", "TABLE", "PLAYER", "STATUS", "RALLY", "TRAIL", "TIME", "TOKENS"
    );
    for t in rows.iter().rev() {
        println!(
            "{:<12} {:<w$} {:<16} {:<15} {:>7} {:<8} {:>6.1}s {:>9}",
            t.id,
            t.name,
            t.player.chars().take(16).collect::<String>(),
            t.status,
            format!("{}/{}", t.rallies, t.max_rallies),
            t.trail,
            t.elapsed_ms(now) as f64 / 1000.0,
            t.tokens
        );
    }
    let points = rows.iter().filter(|t| t.status == "point").count();
    println!("{points}/{} tables scored", rows.len());
}

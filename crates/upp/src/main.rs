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
use upp_core::{api, MatchFile, Scheduler};

const HELP: &str = "upp: orchestrator <-> worker ping-pong tables with an external referee

USAGE:
  upp [--db PATH] <command> [args]

COMMANDS:
  run <match.toml|json>     Run a match in the foreground (exit 0 when every table scores)
      --parallel N            Max tables at once (default: defaults.max_parallel, else 4)
      --watch                 Live TUI while it runs
      --exit-when-done        With --watch: leave the TUI when every table has ended
      --json                  Print the final summary as JSON
  create <match>            Queue a match for a running daemon (uppd) or `upp mcp`
  validate <match>          Parse and check a match file; print the resolved tables
  watch [--run ID] [--all]  Live TUI of all tables (reads the store; works with any runner)
      --exit-when-done        Leave when every shown table has ended
  list [--all] [--json]     Tables of the latest run (--all: every run)
  status <table> [--full]   One table: rallies, referee output, pending ball (JSON)
  pending                   Balls waiting for an external server (JSON)
  answer <table> <accept|return|stop> [note...]
                            Answer a ball as the external server
  stop <table> | --run ID   Stop a table (before its next rally) or a whole run
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
    parallel: Option<usize>,
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
                        .parse()
                        .map_err(|e| format!("--parallel: {e}"))?,
                )
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
            let m = MatchFile::load(Path::new(&path))?;
            let store = Store::open(&db)?;
            print_json(&api::create(&store, m, Some(&path))?);
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
            print_json(&api::answer(
                &store,
                &need(0, "table")?,
                &need(1, "verdict")?,
                &note,
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
        "mcp" => mcp::run(&db, a.parallel.unwrap_or(4), a.queue_only),
        other => Err(format!("unknown command '{other}' (see upp --help)")),
    }
}

fn cmd_run(db: &Path, file: &Path, a: &Args) -> Result<(), String> {
    let m = MatchFile::load(file)?;
    let parallel = a.parallel.or(m.defaults.max_parallel).unwrap_or(4);
    let store = Store::open(db)?;
    let created = api::create(&store, m, Some(&file.to_string_lossy()))?;
    let run_id = created["run_id"].as_str().unwrap_or_default().to_string();
    let mut sched = Scheduler::new(db, parallel, "run");
    sched.run = Some(run_id.clone());
    sched.until_idle = true;
    let stop = sched.stop.clone();
    let owner = sched.owner.clone();
    {
        let db = db.to_path_buf();
        let stop = stop.clone();
        let owner = owner.clone();
        let _ = ctrlc::set_handler(move || {
            stop.store(true, Ordering::SeqCst);
            if let Ok(s) = Store::open(&db) {
                let _ = s.orphan(&owner);
            }
            eprintln!("\nupp: interrupted; running tables marked stopped");
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

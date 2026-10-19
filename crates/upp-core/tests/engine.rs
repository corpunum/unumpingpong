// SPDX-License-Identifier: Apache-2.0
//! Engine tests with scripted fake agents (`shell` player): no models needed.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use upp_core::store::{Store, TableRow};
use upp_core::{api, MatchFile, Scheduler};

fn fake(name: &str) -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fake-agents")
        .join(name);
    std::fs::canonicalize(p)
        .unwrap()
        .to_string_lossy()
        .into_owned()
}

/// Write the match, run it to the end with `parallel` tables at once, return the tables (creation order).
fn run_match(dir: &Path, toml: &str, parallel: usize) -> (Store, Vec<TableRow>) {
    let f = dir.join("match.toml");
    std::fs::write(&f, toml).unwrap();
    let m = MatchFile::load(&f).unwrap();
    let db = dir.join("upp.db");
    let store = Store::open(&db).unwrap();
    let created = api::create(&store, m, None).unwrap();
    let run = created["run_id"].as_str().unwrap().to_string();
    let mut s = Scheduler::new(&db, parallel, "test");
    s.run = Some(run.clone());
    s.until_idle = true;
    s.run().unwrap();
    let mut rows = store.tables(Some(&run), false).unwrap();
    rows.reverse();
    (store, rows)
}

#[test]
fn point_on_rally_two_with_referee_feedback() {
    let d = tempfile::tempdir().unwrap();
    let toml = format!(
        r#"
[[table]]
name = "answer"
ball = "Write the answer to answer.txt"
player = {{ kind = "shell", command = ["sh", "{}"], env = {{ FAKE_LOG = "log" }} }}
referee = [{{ kind = "command", run = "grep -qx 42 answer.txt", name = "answer is 42" }}]
"#,
        fake("fake-coder.sh")
    );
    let (store, rows) = run_match(d.path(), &toml, 2);
    let t = &rows[0];
    assert_eq!(t.status, "point", "{t:?}");
    assert_eq!(t.rallies, 2);
    assert_eq!(t.trail, ".*");
    // The first ball tells the player what the referee checks; the return carries the failure.
    let b1 = std::fs::read_to_string(d.path().join("log/ball-1.txt")).unwrap();
    assert!(b1.contains("grep -qx 42 answer.txt"), "{b1}");
    let b2 = std::fs::read_to_string(d.path().join("log/ball-2.txt")).unwrap();
    assert!(
        b2.contains("Failed checks") && b2.contains("answer is 42"),
        "{b2}"
    );
    // A shell player keeps no session: the brief is repeated.
    assert!(b2.contains("Original ball"), "{b2}");
    let rallies = store.rallies(&t.id).unwrap();
    assert_eq!(rallies.len(), 2);
    assert_eq!(rallies[0].verdict, "return");
    assert_eq!(rallies[1].verdict, "point");
    assert!(t.result.as_deref().unwrap().contains("fixed"));
}

#[test]
fn lost_after_max_rallies() {
    let d = tempfile::tempdir().unwrap();
    let toml = format!(
        r#"
[[table]]
name = "never"
ball = "x"
lease = {{ max_rallies = 2 }}
player = {{ kind = "shell", command = ["sh", "{}"], env = {{ FAKE_PASS_ON = "3" }} }}
referee = [{{ kind = "command", run = "grep -qx 42 answer.txt" }}]
"#,
        fake("fake-coder.sh")
    );
    let (_s, rows) = run_match(d.path(), &toml, 1);
    assert_eq!(rows[0].status, "lost");
    assert_eq!(rows[0].trail, "..");
}

#[test]
fn fault_is_recorded_and_the_ball_is_served_again() {
    let d = tempfile::tempdir().unwrap();
    let toml = format!(
        r#"
[[table]]
name = "crashy"
ball = "x"
player = {{ kind = "shell", command = ["sh", "{}"], env = {{ FAKE_FAIL_ON = "1", FAKE_PASS_ON = "1" }} }}
referee = [{{ kind = "file_exists", path = "answer.txt", contains = "^42" }}]
"#,
        fake("fake-coder.sh")
    );
    let (store, rows) = run_match(d.path(), &toml, 1);
    assert_eq!(rows[0].status, "point");
    assert_eq!(rows[0].trail, "!*");
    let r = store.rallies(&rows[0].id).unwrap();
    assert_eq!(r[0].verdict, "fault");
    assert!(r[0].note.contains("crashed"));
    assert!(r[1].ball.contains("failed before the referee"));
}

#[test]
fn parallel_cap_is_respected() {
    let d = tempfile::tempdir().unwrap();
    let mut toml = format!(
        "[defaults]\nplayer = {{ kind = \"shell\", command = [\"sh\", \"{}\"], env = {{ FAKE_SLEEP = \"0.4\", FAKE_PASS_ON = \"1\" }} }}\n",
        fake("fake-coder.sh")
    );
    for i in 0..5 {
        toml.push_str(&format!(
            "[[table]]\nname = \"t{i}\"\nball = \"x\"\ncwd = \"w{i}\"\nreferee = [{{ kind = \"file_exists\", path = \"answer.txt\" }}]\n"
        ));
        std::fs::create_dir_all(d.path().join(format!("w{i}"))).unwrap();
    }
    let t0 = Instant::now();
    let (_s, rows) = run_match(d.path(), &toml, 2);
    assert!(rows.iter().all(|t| t.status == "point"), "{rows:?}");
    // Max overlap of [started, ended] intervals must be <= 2.
    let mut edges: Vec<(i64, i32)> = Vec::new();
    for t in &rows {
        edges.push((t.started_ms.unwrap(), 1));
        edges.push((t.ended_ms.unwrap(), -1));
    }
    edges.sort();
    let (mut cur, mut max) = (0, 0);
    for (_, d) in edges {
        cur += d;
        max = max.max(cur);
    }
    assert!(max <= 2, "overlap {max}");
    assert!(
        max == 2,
        "tables never overlapped: parallelism not exercised"
    );
    // 5 tables x 0.4 s at 2 wide: >= 1.2 s, well under serial 2.0 s + overhead.
    assert!(t0.elapsed() >= Duration::from_millis(1100));
}

#[test]
fn serial_chain_passes_the_point_downstream() {
    let d = tempfile::tempdir().unwrap();
    let toml = format!(
        r#"
[defaults]
player = {{ kind = "shell", command = ["sh", "{}"] }}

[[table]]
name = "plan"
ball = "PLAN-BALL make a plan"

[[table]]
name = "implement"
after = "plan"
ball = "IMPL got: {{{{prev.answer}}}}"

[[table]]
name = "test"
after = "implement"
ball = "TEST got: {{{{prev.answer}}}}"
"#,
        fake("echo-agent.sh")
    );
    let (_s, rows) = run_match(d.path(), &toml, 4);
    assert!(rows.iter().all(|t| t.status == "point"), "{rows:?}");
    let res = |i: usize| rows[i].result.clone().unwrap();
    assert!(res(0).contains("PLAN-BALL"));
    assert!(res(1).contains("IMPL got:") && res(1).contains(&rows[0].id));
    assert!(res(2).contains("TEST got:") && res(2).contains("IMPL got:"));
    // Serial: each starts after its upstream ended.
    assert!(rows[1].started_ms.unwrap() >= rows[0].ended_ms.unwrap());
    assert!(rows[2].started_ms.unwrap() >= rows[1].ended_ms.unwrap());
}

#[test]
fn failed_upstream_skips_downstream() {
    let d = tempfile::tempdir().unwrap();
    let toml = format!(
        r#"
[defaults]
player = {{ kind = "shell", command = ["sh", "{}"] }}
[[table]]
name = "a"
ball = "x"
lease = {{ max_rallies = 1 }}
referee = [{{ kind = "command", run = "false" }}]
[[table]]
name = "b"
after = "a"
ball = "y"
"#,
        fake("echo-agent.sh")
    );
    let (_s, rows) = run_match(d.path(), &toml, 2);
    assert_eq!(rows[0].status, "lost");
    assert_eq!(rows[1].status, "skipped");
}

#[test]
fn wall_lease_expires_a_slow_player() {
    let d = tempfile::tempdir().unwrap();
    let toml = format!(
        r#"
[[table]]
name = "slow"
ball = "x"
lease = {{ wall_secs = 1, max_rallies = 5 }}
player = {{ kind = "shell", command = ["sh", "{}"], env = {{ FAKE_SLEEP = "5" }} }}
referee = [{{ kind = "command", run = "true" }}]
"#,
        fake("fake-coder.sh")
    );
    let t0 = Instant::now();
    let (store, rows) = run_match(d.path(), &toml, 1);
    assert_eq!(rows[0].status, "expired", "{:?}", rows[0]);
    assert!(t0.elapsed() < Duration::from_secs(4), "{:?}", t0.elapsed());
    // The timed-out call is a recorded fault.
    assert_eq!(store.rallies(&rows[0].id).unwrap()[0].verdict, "fault");
}

#[test]
fn model_server_writes_the_note() {
    let d = tempfile::tempdir().unwrap();
    let toml = format!(
        r#"
[[table]]
name = "served"
ball = "x"
player = {{ kind = "shell", command = ["sh", "{}"], env = {{ FAKE_LOG = "log" }} }}
referee = [{{ kind = "command", run = "grep -qx 42 answer.txt" }}]
[table.server]
mode = "model"
player = {{ kind = "shell", command = "echo 'SERVER-NOTE: the file must contain exactly 42'" }}
"#,
        fake("fake-coder.sh")
    );
    let (_s, rows) = run_match(d.path(), &toml, 1);
    assert_eq!(rows[0].status, "point");
    let b2 = std::fs::read_to_string(d.path().join("log/ball-2.txt")).unwrap();
    assert!(b2.contains("Server note: SERVER-NOTE"), "{b2}");
}

#[test]
fn stop_request_ends_a_running_table() {
    let d = tempfile::tempdir().unwrap();
    let toml = format!(
        r#"
[[table]]
name = "long"
ball = "x"
lease = {{ max_rallies = 50 }}
player = {{ kind = "shell", command = ["sh", "{}"], env = {{ FAKE_SLEEP = "0.2", FAKE_PASS_ON = "99" }} }}
referee = [{{ kind = "command", run = "false" }}]
"#,
        fake("fake-coder.sh")
    );
    let f = d.path().join("m.toml");
    std::fs::write(&f, toml).unwrap();
    let db = d.path().join("upp.db");
    let store = Store::open(&db).unwrap();
    let c = api::create(&store, MatchFile::load(&f).unwrap(), None).unwrap();
    let id = c["tables"][0]["id"].as_str().unwrap().to_string();
    let mut s = Scheduler::new(&db, 1, "test");
    s.until_idle = true;
    s.run = Some(c["run_id"].as_str().unwrap().into());
    let h = std::thread::spawn(move || s.run());
    std::thread::sleep(Duration::from_millis(700));
    api::stop(&store, &id).unwrap();
    h.join().unwrap().unwrap();
    let t = store.table(&id).unwrap().unwrap();
    assert_eq!(t.status, "stopped");
    assert!(t.rallies >= 1 && t.rallies < 50);
}

#[test]
fn export_has_one_line_per_rally() {
    let d = tempfile::tempdir().unwrap();
    let toml = format!(
        r#"
[[table]]
name = "e"
ball = "x"
player = {{ kind = "shell", command = ["sh", "{}"], env = {{ FAKE_PASS_ON = "3" }} }}
referee = [{{ kind = "command", run = "grep -qx 42 answer.txt" }}]
"#,
        fake("fake-coder.sh")
    );
    let (store, rows) = run_match(d.path(), &toml, 1);
    let out = api::export_jsonl(&store, Some(&rows[0].run_id)).unwrap();
    let lines: Vec<serde_json::Value> = out
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(lines.len(), 3);
    assert_eq!(lines[2]["verdict"], "point");
    assert_eq!(lines[0]["table"], "e");
    assert!(lines[0]["referee"].is_array());
}

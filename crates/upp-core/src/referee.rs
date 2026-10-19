// SPDX-License-Identifier: Apache-2.0
//! The referee: deterministic checks first (commands, files, HTTP, JSON values),
//! an LLM judge only as a last resort. The player never grades itself.

use crate::config::{Check, CheckSpec};
use crate::player::{self, PlayCtx};
use crate::proc;
use regex::Regex;
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Output kept per check (tail), enough to fix the problem, small enough for a fresh context.
pub const OUTPUT_TAIL: usize = 6000;

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct CheckResult {
    pub name: String,
    pub kind: String,
    pub pass: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub skipped: bool,
    /// One line: what was checked and the outcome.
    pub summary: String,
    /// Evidence: command output tail, HTTP body excerpt, judge reply.
    pub output: String,
    pub duration_ms: u64,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Verdict {
    pub passed: bool,
    pub checks: Vec<CheckResult>,
}

impl Verdict {
    /// Human/LLM-readable evidence block for the return message.
    pub fn report(&self, only_failed: bool) -> String {
        let mut s = String::new();
        for c in &self.checks {
            if only_failed && (c.pass || c.skipped) {
                continue;
            }
            let mark = if c.skipped {
                "SKIP"
            } else if c.pass {
                "PASS"
            } else {
                "FAIL"
            };
            s.push_str(&format!("- [{mark}] {}: {}\n", c.name, c.summary));
            if !c.output.trim().is_empty() && !c.pass {
                s.push_str("```\n");
                s.push_str(c.output.trim_end());
                s.push_str("\n```\n");
            }
        }
        s
    }
}

/// First word of a reply, upper-cased, ignoring markdown decoration (`**PASS**`, `# FAIL`).
/// `PASSED` or `PASSABLE` is not `PASS`.
pub fn first_word(text: &str) -> String {
    text.trim_start()
        .trim_start_matches(['*', '#', '`', '"', '\'', ' ', '>', '_'])
        .chars()
        .take_while(|c| c.is_ascii_alphabetic())
        .collect::<String>()
        .to_ascii_uppercase()
}

pub struct RefCtx<'a> {
    pub table_id: &'a str,
    pub rally: u32,
    pub cwd: &'a Path,
    pub goal: &'a str,
    pub ball: &'a str,
    pub answer: &'a str,
    pub deadline: Instant,
}

fn remaining(deadline: Instant, cap: Duration) -> Duration {
    deadline
        .saturating_duration_since(Instant::now())
        .min(cap)
        .max(Duration::from_millis(200))
}

fn answer_file(ctx: &RefCtx) -> Option<PathBuf> {
    let dir = std::env::temp_dir().join("unumpingpong");
    std::fs::create_dir_all(&dir).ok()?;
    let uniq = uuid::Uuid::new_v4().simple().to_string();
    let p = dir.join(format!(
        "{}-{}-{}.answer.txt",
        ctx.table_id,
        ctx.rally,
        &uniq[..8]
    ));
    std::fs::write(&p, ctx.answer).ok()?;
    Some(p)
}

/// The verdict of a table that opted in to `unverified = true` and has no checks: it
/// passes, and the check row says plainly that nothing was verified.
pub fn unverified() -> Verdict {
    Verdict {
        passed: true,
        checks: vec![CheckResult {
            name: "unverified".into(),
            kind: "unverified".into(),
            pass: true,
            skipped: false,
            summary:
                "UNVERIFIED: no referee checks (unverified = true); this point carries no evidence"
                    .into(),
            output: String::new(),
            duration_ms: 0,
        }],
    }
}

/// Run every check. Deterministic checks run in order; judges run only if all of them passed.
/// An empty check list fails: a point always needs evidence (see [`unverified`] for the opt-in).
pub fn referee(checks: &[CheckSpec], ctx: &RefCtx) -> Verdict {
    if checks.is_empty() {
        return Verdict {
            passed: false,
            checks: vec![CheckResult {
                name: "none".into(),
                kind: "none".into(),
                pass: false,
                skipped: false,
                summary: "no referee checks configured: nothing verifies this answer".into(),
                output: String::new(),
                duration_ms: 0,
            }],
        };
    }
    let ans_file = answer_file(ctx);
    let mut results = Vec::new();
    let (det, judges): (Vec<_>, Vec<_>) = checks
        .iter()
        .partition(|c| !matches!(c.check, Check::Judge { .. }));
    for c in &det {
        results.push(run_check(c, ctx, ans_file.as_deref()));
    }
    let det_ok = results.iter().all(|r| r.pass);
    for c in &judges {
        if det_ok {
            results.push(run_check(c, ctx, ans_file.as_deref()));
        } else {
            results.push(CheckResult {
                name: name_of(c),
                kind: "judge".into(),
                pass: false,
                skipped: true,
                summary: "skipped: a deterministic check failed".into(),
                output: String::new(),
                duration_ms: 0,
            });
        }
    }
    if let Some(p) = ans_file {
        let _ = std::fs::remove_file(p);
    }
    Verdict {
        passed: results.iter().all(|r| r.pass),
        checks: results,
    }
}

fn name_of(c: &CheckSpec) -> String {
    if let Some(n) = &c.name {
        return n.clone();
    }
    match &c.check {
        Check::Command { run, .. } => proc::head(&run.display(), 60),
        Check::FileExists { path, .. } => format!("file {path}"),
        Check::HttpOk { url, .. } => format!("GET {url}"),
        Check::JsonPathEquals { path, .. } => format!("json {path}"),
        Check::Judge { player, .. } => format!("judge {}", player.label()),
    }
}

fn run_check(c: &CheckSpec, ctx: &RefCtx, ans_file: Option<&Path>) -> CheckResult {
    let start = Instant::now();
    let name = name_of(c);
    let kind = c.check.kind().to_string();
    let (pass, summary, output) = match &c.check {
        Check::Command {
            run,
            timeout_secs,
            expect_regex,
            cwd,
            env,
        } => {
            let dir = match cwd {
                Some(d) => ctx.cwd.join(d),
                None => ctx.cwd.to_path_buf(),
            };
            let mut env: BTreeMap<String, String> = env.clone();
            env.insert("UPP_TABLE".into(), ctx.table_id.into());
            env.insert("UPP_RALLY".into(), ctx.rally.to_string());
            if let Some(p) = ans_file {
                env.insert("UPP_ANSWER_FILE".into(), p.to_string_lossy().into_owned());
            }
            let timeout = remaining(
                ctx.deadline,
                Duration::from_secs(timeout_secs.unwrap_or(300)),
            );
            match proc::run(&run.argv(), &dir, None, &env, timeout) {
                Err(e) => (false, e, String::new()),
                Ok(o) => {
                    let out = o.combined();
                    let mut pass = o.ok();
                    let mut summary = if o.timed_out {
                        format!("timed out after {:.1}s", o.duration_ms as f64 / 1000.0)
                    } else {
                        format!(
                            "exit {} in {:.1}s",
                            o.code.map(|c| c.to_string()).unwrap_or("signal".into()),
                            o.duration_ms as f64 / 1000.0
                        )
                    };
                    if pass {
                        if let Some(r) = expect_regex {
                            match Regex::new(r) {
                                Ok(re) if re.is_match(&out) => {}
                                Ok(_) => {
                                    pass = false;
                                    summary.push_str(&format!(", output does not match /{r}/"));
                                }
                                Err(e) => {
                                    pass = false;
                                    summary.push_str(&format!(", bad regex: {e}"));
                                }
                            }
                        }
                    }
                    (pass, summary, proc::tail(&out, OUTPUT_TAIL))
                }
            }
        }
        Check::FileExists { path, contains } => {
            let p = ctx.cwd.join(path);
            match std::fs::read(&p) {
                Err(e) => (
                    false,
                    format!("{} missing ({e})", p.display()),
                    String::new(),
                ),
                Ok(bytes) => match contains {
                    None => (
                        true,
                        format!("{} exists ({} bytes)", path, bytes.len()),
                        String::new(),
                    ),
                    Some(r) => {
                        let text = String::from_utf8_lossy(&bytes);
                        match Regex::new(r) {
                            Ok(re) if re.is_match(&text) => {
                                (true, format!("{path} matches /{r}/"), String::new())
                            }
                            Ok(_) => (
                                false,
                                format!("{path} does not match /{r}/"),
                                proc::head(&text, 1500),
                            ),
                            Err(e) => (false, format!("bad regex: {e}"), String::new()),
                        }
                    }
                },
            }
        }
        Check::HttpOk {
            url,
            status,
            expect_regex,
            timeout_secs,
        } => {
            let t = remaining(
                ctx.deadline,
                Duration::from_secs(timeout_secs.unwrap_or(30)),
            );
            let agent = ureq::AgentBuilder::new().timeout(t).build();
            let (code, body) = match agent.get(url).call() {
                Ok(r) => (r.status(), r.into_string().unwrap_or_default()),
                Err(ureq::Error::Status(c, r)) => (c, r.into_string().unwrap_or_default()),
                Err(e) => (0, e.to_string()),
            };
            let mut pass = match status {
                Some(s) => code == *s,
                None => (200..300).contains(&code),
            };
            let mut summary = format!("HTTP {code}");
            if pass {
                if let Some(r) = expect_regex {
                    if !Regex::new(r).map(|re| re.is_match(&body)).unwrap_or(false) {
                        pass = false;
                        summary.push_str(&format!(", body does not match /{r}/"));
                    }
                }
            }
            (pass, summary, proc::head(&body, 1500))
        }
        Check::JsonPathEquals {
            path,
            equals,
            file,
            url,
        } => {
            let src: Result<Value, String> = if let Some(f) = file {
                std::fs::read_to_string(ctx.cwd.join(f))
                    .map_err(|e| format!("cannot read {f}: {e}"))
                    .and_then(|t| serde_json::from_str(&t).map_err(|e| format!("{f}: {e}")))
            } else if let Some(u) = url {
                ureq::get(u)
                    .timeout(remaining(ctx.deadline, Duration::from_secs(30)))
                    .call()
                    .map_err(|e| e.to_string())
                    .and_then(|r| r.into_json::<Value>().map_err(|e| e.to_string()))
            } else {
                extract_json(ctx.answer).ok_or_else(|| "the answer holds no JSON value".into())
            };
            match src {
                Err(e) => (false, e, String::new()),
                Ok(v) => match json_path(&v, path) {
                    None => (
                        false,
                        format!("{path} not found"),
                        proc::head(&v.to_string(), 1500),
                    ),
                    Some(got) if got == equals => {
                        (true, format!("{path} == {equals}"), String::new())
                    }
                    Some(got) => (
                        false,
                        format!("{path} is {got}, expected {equals}"),
                        String::new(),
                    ),
                },
            }
        }
        Check::Judge {
            player: spec,
            rubric,
        } => {
            let prompt = format!(
                "You are a strict referee. Decide whether the worker's answer achieves the goal.\n\
                 Reply with PASS or FAIL as the first word, then one or two lines of reasons.\n\n\
                 Goal:\n{}\n\nBall (task):\n{}\n\n{}Worker's answer:\n{}\n",
                ctx.goal,
                proc::head(ctx.ball, 6000),
                rubric
                    .as_ref()
                    .map(|r| format!("Rubric:\n{r}\n\n"))
                    .unwrap_or_default(),
                proc::head(ctx.answer, 12000)
            );
            let pctx = PlayCtx {
                table_id: ctx.table_id.to_string(),
                rally: ctx.rally,
                cwd: ctx.cwd.to_path_buf(),
                deadline: ctx.deadline,
                session_key: format!("{}-judge-{}", ctx.table_id, ctx.rally),
            };
            match player::build(spec, &pctx).and_then(|mut p| p.play(&prompt, &pctx)) {
                Err(e) => (false, format!("judge error: {e}"), String::new()),
                Ok(r) => {
                    let pass = first_word(&r.text) == "PASS";
                    (
                        pass,
                        format!("judge says {}", if pass { "PASS" } else { "FAIL" }),
                        proc::head(r.text.trim(), 1500),
                    )
                }
            }
        }
    };
    CheckResult {
        name,
        kind,
        pass,
        skipped: false,
        summary,
        output,
        duration_ms: start.elapsed().as_millis() as u64,
    }
}

/// Parse the whole text as JSON, else the first balanced `{...}` or `[...]` block in it.
pub fn extract_json(text: &str) -> Option<Value> {
    let t = text.trim();
    if let Ok(v) = serde_json::from_str::<Value>(t) {
        return Some(v);
    }
    let bytes = t.as_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        if b == b'{' || b == b'[' {
            let mut de = serde_json::Deserializer::from_str(&t[i..]).into_iter::<Value>();
            if let Some(Ok(v)) = de.next() {
                return Some(v);
            }
        }
    }
    None
}

/// Dotted path: `a.b.0.c`, also `$.a.b[0].c`.
pub fn json_path<'a>(v: &'a Value, path: &str) -> Option<&'a Value> {
    let p = path.trim().trim_start_matches('$').trim_start_matches('.');
    let p = p.replace('[', ".").replace(']', "");
    let mut cur = v;
    for seg in p.split('.').filter(|s| !s.is_empty()) {
        cur = match cur {
            Value::Object(m) => m.get(seg)?,
            Value::Array(a) => a.get(seg.parse::<usize>().ok()?)?,
            _ => return None,
        };
    }
    Some(cur)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Cmd;
    use serde_json::json;

    fn ctx<'a>(cwd: &'a Path, answer: &'a str) -> RefCtx<'a> {
        RefCtx {
            table_id: "t1",
            rally: 1,
            cwd,
            goal: "g",
            ball: "b",
            answer,
            deadline: Instant::now() + Duration::from_secs(60),
        }
    }

    fn cmd(run: &str, timeout: Option<u64>, re: Option<&str>) -> CheckSpec {
        CheckSpec {
            name: None,
            check: Check::Command {
                run: Cmd::Line(run.into()),
                timeout_secs: timeout,
                expect_regex: re.map(String::from),
                cwd: None,
                env: BTreeMap::new(),
            },
        }
    }

    #[test]
    fn no_checks_never_pass_and_judge_words_are_strict() {
        let d = std::env::temp_dir();
        let v = referee(&[], &ctx(&d, "I did it, trust me"));
        assert!(!v.passed, "an empty check list must not pass");
        assert!(!v.checks[0].pass);
        let u = unverified();
        assert!(u.passed && u.checks[0].summary.contains("UNVERIFIED"));
        assert_eq!(first_word("**PASS** looks right"), "PASS");
        assert_eq!(first_word("> pass: ok"), "PASS");
        assert_ne!(first_word("PASSED? no, FAIL"), "PASS");
        assert_ne!(first_word("Passable but FAIL"), "PASS");
        assert_eq!(first_word("FAIL because"), "FAIL");
    }

    #[test]
    fn json_helpers() {
        let v = json!({"a": {"b": [1, {"c": "x"}]}});
        assert_eq!(json_path(&v, "a.b.1.c"), Some(&json!("x")));
        assert_eq!(json_path(&v, "$.a.b[0]"), Some(&json!(1)));
        assert_eq!(json_path(&v, "a.z"), None);
        assert_eq!(
            extract_json("Here you go:\n```json\n{\"ok\": true}\n```"),
            Some(json!({"ok": true}))
        );
    }

    #[cfg(unix)]
    #[test]
    fn command_checks_regex_and_answer_file() {
        let d = tempfile::tempdir().unwrap();
        let v = referee(
            &[
                cmd("echo 'test result: ok'", None, Some("result: ok")),
                cmd("grep -q 42 \"$UPP_ANSWER_FILE\"", None, None),
            ],
            &ctx(d.path(), "the answer is 42"),
        );
        assert!(v.passed, "{v:?}");
        let v = referee(&[cmd("echo nope; exit 1", None, None)], &ctx(d.path(), ""));
        assert!(!v.passed);
        assert!(v.report(true).contains("nope"));
    }

    #[cfg(unix)]
    #[test]
    fn command_timeout_fails_fast() {
        let d = tempfile::tempdir().unwrap();
        let t = Instant::now();
        let v = referee(&[cmd("sleep 20", Some(1), None)], &ctx(d.path(), ""));
        assert!(!v.passed);
        assert!(
            v.checks[0].summary.contains("timed out"),
            "{:?}",
            v.checks[0]
        );
        assert!(t.elapsed() < Duration::from_secs(6));
    }

    #[test]
    fn file_and_json_checks() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("out.txt"), "hello world").unwrap();
        std::fs::write(d.path().join("r.json"), r#"{"tests": {"failed": 0}}"#).unwrap();
        let checks = vec![
            CheckSpec {
                name: None,
                check: Check::FileExists {
                    path: "out.txt".into(),
                    contains: Some("wor.d".into()),
                },
            },
            CheckSpec {
                name: None,
                check: Check::JsonPathEquals {
                    path: "tests.failed".into(),
                    equals: json!(0),
                    file: Some("r.json".into()),
                    url: None,
                },
            },
            CheckSpec {
                name: None,
                check: Check::JsonPathEquals {
                    path: "status".into(),
                    equals: json!("done"),
                    file: None,
                    url: None,
                },
            },
        ];
        let v = referee(&checks, &ctx(d.path(), "ok: {\"status\": \"done\"}"));
        assert!(v.passed, "{v:?}");
        let v = referee(&checks, &ctx(d.path(), "{\"status\": \"wip\"}"));
        assert!(!v.passed);
        assert!(v.checks[2].summary.contains("expected"));
    }

    #[cfg(unix)]
    #[test]
    fn judge_runs_last_and_only_after_deterministic_pass() {
        let d = tempfile::tempdir().unwrap();
        let judge = CheckSpec {
            name: Some("judge".into()),
            check: Check::Judge {
                player: crate::config::PlayerSpec::Shell {
                    command: Cmd::Line("echo 'PASS looks right'".into()),
                    timeout_secs: None,
                    env: BTreeMap::new(),
                },
                rubric: None,
            },
        };
        let v = referee(
            &[judge.clone(), cmd("true", None, None)],
            &ctx(d.path(), "a"),
        );
        assert!(v.passed);
        assert_eq!(v.checks.last().unwrap().kind, "judge");
        let v = referee(&[judge, cmd("false", None, None)], &ctx(d.path(), "a"));
        assert!(!v.passed);
        assert!(v.checks[1].skipped);
    }
}

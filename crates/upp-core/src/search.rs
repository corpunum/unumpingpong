// SPDX-License-Identifier: Apache-2.0
//! Code search for players without a tool loop: `{{search:query}}` in a brief and a table's
//! `context_search` run [UnumSearch](https://github.com/corpunum/unumsearch) (`unumsearch search`,
//! JSON output) in the table's cwd and put the top hits into the ball, capped.
//!
//! The binary is `unumsearch` on PATH, or `$UPP_UNUMSEARCH`. A failed search becomes a one-line
//! note in the brief, never a failed table.

use crate::proc;
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

/// Hits kept per query.
pub const MAX_HITS: usize = 8;
/// Characters kept per hit line.
pub const LINE_MAX: usize = 200;
/// Bytes of hits per query.
pub const QUERY_MAX: usize = 2000;

pub fn bin() -> String {
    std::env::var("UPP_UNUMSEARCH")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "unumsearch".into())
}

/// The top hits of a literal search in `cwd`, one `path:line: text` per line (paths relative
/// to `cwd`), or a note saying why there are none.
pub fn hits(query: &str, cwd: &Path) -> String {
    let argv = vec![
        bin(),
        "search".into(),
        "-F".into(),
        "-m".into(),
        MAX_HITS.to_string(),
        query.to_string(),
        cwd.to_string_lossy().into_owned(),
    ];
    let out = match proc::run(&argv, cwd, None, &BTreeMap::new(), Duration::from_secs(30)) {
        Ok(o) if o.ok() => o,
        Ok(o) => {
            return format!(
                "(unumsearch failed: {})",
                proc::head(o.combined().trim(), 200)
            )
        }
        Err(e) => return format!("(unumsearch unavailable: {})", proc::head(&e, 200)),
    };
    let v: Value = match serde_json::from_str(out.stdout.trim()) {
        Ok(v) => v,
        Err(e) => return format!("(unumsearch output not JSON: {e})"),
    };
    if v["ok"] == false {
        return format!(
            "(unumsearch error: {})",
            proc::head(&v["error"].to_string(), 200)
        );
    }
    let matches = v["result"]["matches"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    if matches.is_empty() {
        return "(no matches)".into();
    }
    let mut s = String::new();
    for m in matches.iter().take(MAX_HITS) {
        let path = m["path"].as_str().unwrap_or("?");
        let rel = Path::new(path)
            .strip_prefix(cwd)
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|_| path.to_string());
        let text: String = m["text"]
            .as_str()
            .unwrap_or("")
            .trim()
            .chars()
            .take(LINE_MAX)
            .collect();
        let line = format!("{rel}:{}: {text}\n", m["line"]);
        if s.len() + line.len() > QUERY_MAX {
            s.push_str("[... more hits cut ...]\n");
            break;
        }
        s.push_str(&line);
    }
    if v["result"]["truncated"] == true || matches.len() > MAX_HITS {
        s.push_str(&format!("(first {MAX_HITS} hits)\n"));
    }
    s.trim_end().to_string()
}

/// Replace every `{{search:query}}` with a fenced block of its hits.
pub fn render(s: &str, cwd: &Path) -> String {
    let re = regex::Regex::new(r"\{\{search:([^}]+)\}\}").expect("static regex");
    re.replace_all(s, |c: &regex::Captures| {
        let q = c[1].trim();
        format!("\n```text\n# unumsearch: {q}\n{}\n```\n", hits(q, cwd))
    })
    .into_owned()
}

/// The brief section for a table's `context_search` queries (empty for none).
pub fn context(queries: &[String], cwd: &Path) -> String {
    let queries: Vec<&String> = queries.iter().filter(|q| !q.trim().is_empty()).collect();
    if queries.is_empty() {
        return String::new();
    }
    let mut s =
        String::from("\n\n---\nCode search results (unumsearch, in this table's directory):\n");
    for q in queries {
        s.push_str(&format!(
            "```text\n# {}\n{}\n```\n",
            q.trim(),
            hits(q.trim(), cwd)
        ));
    }
    s.trim_end().to_string()
}

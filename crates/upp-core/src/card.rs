// SPDX-License-Identifier: Apache-2.0
//! Result cards: every rally's player output condensed to a compact, structured card.
//!
//! The orchestrator reads cards and referee output, never transcripts (those stay in
//! the store and are fetched on demand). A card comes from the structured tail block
//! the player is asked to end its answer with:
//!
//! ```text
//! <card>
//! done: yes
//! summary: parse() handles quoted fields; all 14 tests pass
//! evidence: python3 -m unittest -q -> OK (14 tests)
//! open_issues: none
//! artifacts: solution.py; commit 3f2a9c1
//! </card>
//! ```
//!
//! When the block is missing or unreadable, a deterministic extractor builds the card
//! from the answer text (`source = "extracted"`). The engine then adds what it knows
//! for certain: tokens, elapsed time, the referee result, files it wrote, provenance.

use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::OnceLock;

/// Instruction appended to the first ball when cards are on.
pub const CARD_INSTRUCTION: &str =
    "End your answer with a short result card. The orchestrator reads only this card and \
the referee output, not your full answer:\n<card>\ndone: yes|no\nsummary: one line\n\
evidence: what you ran or checked, and the outcome\nopen_issues: none, or what is left\n\
artifacts: files, commits or PR links you produced\n</card>";

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct Card {
    /// The player's own claim. The referee result (`referee.passed`) is the truth.
    pub done: bool,
    pub summary: String,
    #[serde(default)]
    pub evidence: Vec<String>,
    #[serde(default)]
    pub open_issues: Vec<String>,
    #[serde(default)]
    pub artifacts: Vec<String>,
    /// `block` (the player's card), `extracted` (built from the answer) or `engine`.
    pub source: String,
}

const MAX_ITEMS: usize = 6;
const MAX_ITEM: usize = 240;

fn clip(s: &str, n: usize) -> String {
    let s = s.trim();
    if s.chars().count() <= n {
        s.to_string()
    } else {
        format!("{}...", s.chars().take(n).collect::<String>())
    }
}

fn is_none_word(s: &str) -> bool {
    matches!(
        s.trim()
            .trim_matches(|c: char| c == '.' || c == '-')
            .to_ascii_lowercase()
            .as_str(),
        "" | "none" | "n/a" | "na" | "nothing" | "no" | "[]" | "-"
    )
}

fn push_item(v: &mut Vec<String>, s: &str) {
    let s = s.trim().trim_start_matches(['-', '*']).trim();
    if !is_none_word(s) && v.len() < MAX_ITEMS && !v.iter().any(|x| x == s) {
        v.push(clip(s, MAX_ITEM));
    }
}

fn truthy(s: &str) -> bool {
    matches!(
        s.trim()
            .trim_end_matches('.')
            .to_ascii_lowercase()
            .split_whitespace()
            .next()
            .unwrap_or(""),
        "yes" | "true" | "y" | "done" | "complete" | "completed"
    )
}

fn block_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?is)<card>(.*?)(?:</card>|\z)").expect("static regex"))
}

/// The answer without its `<card>...</card>` block(s).
pub fn strip(answer: &str) -> String {
    block_re().replace_all(answer, "").trim_end().to_string()
}

/// Parse the last `<card>` block of an answer, if any.
pub fn parse_block(answer: &str) -> Option<Card> {
    let body = block_re().captures_iter(answer).last()?.get(1)?.as_str();
    let body = body.trim();
    if body.starts_with('{') {
        let v: Value = serde_json::from_str(body).ok()?;
        let list = |k: &str| -> Vec<String> {
            let mut out = Vec::new();
            match &v[k] {
                Value::Array(a) => a
                    .iter()
                    .filter_map(|x| x.as_str())
                    .for_each(|x| push_item(&mut out, x)),
                Value::String(s) => push_item(&mut out, s),
                _ => {}
            }
            out
        };
        return Some(Card {
            done: match &v["done"] {
                Value::Bool(b) => *b,
                Value::String(s) => truthy(s),
                _ => false,
            },
            summary: clip(v["summary"].as_str().unwrap_or(""), MAX_ITEM),
            evidence: list("evidence"),
            open_issues: list("open_issues"),
            artifacts: list("artifacts"),
            source: "block".into(),
        });
    }
    let mut c = Card {
        source: "block".into(),
        ..Default::default()
    };
    let mut cur = String::new();
    let mut any = false;
    for line in body.lines() {
        let t = line.trim();
        if t.is_empty() {
            continue;
        }
        let key_val = t.split_once(':').and_then(|(k, v)| {
            let k = k.trim().to_ascii_lowercase().replace([' ', '-'], "_");
            let k = match k.as_str() {
                "done" | "status" => "done",
                "summary" | "what" | "result" => "summary",
                "evidence" | "checks" | "verified" => "evidence",
                "open_issues" | "issues" | "open" | "todo" | "remaining" => "open_issues",
                "artifacts" | "artifact" | "files" | "outputs" => "artifacts",
                _ => return None,
            };
            Some((k, v.trim()))
        });
        match key_val {
            Some((k, v)) => {
                any = true;
                cur = k.to_string();
                match k {
                    "done" => c.done = truthy(v),
                    "summary" => c.summary = clip(v, MAX_ITEM),
                    _ => {
                        let target = list_mut(&mut c, k);
                        for part in v.split("; ") {
                            push_item(target, part);
                        }
                    }
                }
            }
            None => match cur.as_str() {
                "evidence" | "open_issues" | "artifacts" => {
                    push_item(list_mut(&mut c, &cur.clone()), t);
                }
                "summary" if c.summary.len() < MAX_ITEM => {
                    c.summary = clip(&format!("{} {t}", c.summary), MAX_ITEM);
                }
                _ => {}
            },
        }
    }
    any.then_some(c)
}

fn list_mut<'a>(c: &'a mut Card, k: &str) -> &'a mut Vec<String> {
    match k {
        "evidence" => &mut c.evidence,
        "open_issues" => &mut c.open_issues,
        _ => &mut c.artifacts,
    }
}

/// Build a card from free text, deterministically: the fallback when there is no block.
pub fn extract(answer: &str) -> Card {
    static PR: OnceLock<Regex> = OnceLock::new();
    static SHA: OnceLock<Regex> = OnceLock::new();
    static PATH: OnceLock<Regex> = OnceLock::new();
    let pr = PR.get_or_init(|| {
        Regex::new(r"https?://[^\s)>\]]+/(?:pull|merge_requests)/\d+").expect("static regex")
    });
    let sha = SHA.get_or_init(|| {
        Regex::new(r"(?i)\bcommit(?:ted)?\b[^0-9a-f\n]{0,20}\b([0-9a-f]{7,40})\b")
            .expect("static regex")
    });
    let path = PATH.get_or_init(|| {
        Regex::new(r"(?:^|[\s`(])((?:[\w.-]+/)*[\w.-]+\.(?:rs|py|js|mjs|ts|tsx|go|java|c|h|cpp|hpp|rb|sh|toml|json|yaml|yml|md|txt|html|css|sql))\b")
            .expect("static regex")
    });
    let text = strip(answer);
    // Prose only: code blocks are the work itself, not a description of it.
    let mut prose = String::new();
    let mut in_code = false;
    for l in text.lines() {
        if l.trim_start().starts_with("```") {
            in_code = !in_code;
            continue;
        }
        if !in_code {
            prose.push_str(l);
            prose.push('\n');
        }
    }
    let lower = prose.to_ascii_lowercase();
    let mut c = Card {
        source: "extracted".into(),
        ..Default::default()
    };
    c.summary = clip(
        prose
            .lines()
            .map(|l| l.trim().trim_start_matches(['#', '*', '-', '>']).trim())
            .find(|l| !l.is_empty())
            .unwrap_or(""),
        MAX_ITEM,
    );
    let negative = [
        "could not",
        "couldn't",
        "unable to",
        "not yet",
        "still fail",
        "blocked",
        "cannot ",
        "can't ",
        "did not",
        "didn't",
    ];
    let positive = [
        "done",
        "fixed",
        "implemented",
        "pass",
        "complete",
        "finished",
        "works",
        "added",
        "wrote",
    ];
    for l in prose.lines() {
        let ll = l.to_ascii_lowercase();
        if [
            "test", "pass", "exit ", "ok (", "verified", "ran ", "checked", "build",
        ]
        .iter()
        .any(|w| ll.contains(w))
        {
            push_item(&mut c.evidence, l);
        }
        if [
            "todo",
            "fixme",
            "not yet",
            "remaining",
            "unable",
            "could not",
            "known issue",
            "left to",
        ]
        .iter()
        .any(|w| ll.contains(w))
        {
            push_item(&mut c.open_issues, l);
        }
    }
    // A claim of done: positive words, no negative ones, and no open issue left.
    c.done = !negative.iter().any(|w| lower.contains(w))
        && positive.iter().any(|w| lower.contains(w))
        && c.open_issues.is_empty();
    for m in pr.find_iter(&text) {
        push_item(&mut c.artifacts, m.as_str());
    }
    for m in sha.captures_iter(&prose) {
        push_item(&mut c.artifacts, &format!("commit {}", &m[1]));
    }
    for m in path.captures_iter(&prose) {
        push_item(&mut c.artifacts, &m[1]);
    }
    c
}

/// The card for an answer: the player's block when it has one, else the extractor's.
pub fn from_answer(answer: &str) -> Card {
    parse_block(answer).unwrap_or_else(|| extract(answer))
}

/// Rough token estimate (bytes / 4) used for orchestrator-side reading costs.
pub fn est_tokens(s: &str) -> u64 {
    (s.len() as u64).div_ceil(4)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_tail_block() {
        let a = "Here is the fix.\n```python\ndef f(): pass\n```\n<card>\ndone: yes\nsummary: f() handles \
                 empty input\nevidence: python3 -m unittest -> OK (3 tests)\nopen_issues: none\nartifacts: \
                 solution.py; commit abc1234\n</card>\n";
        let c = parse_block(a).unwrap();
        assert!(c.done);
        assert_eq!(c.source, "block");
        assert_eq!(c.summary, "f() handles empty input");
        assert_eq!(c.evidence, vec!["python3 -m unittest -> OK (3 tests)"]);
        assert!(c.open_issues.is_empty());
        assert_eq!(c.artifacts, vec!["solution.py", "commit abc1234"]);
        assert_eq!(strip(a), "Here is the fix.\n```python\ndef f(): pass\n```");
    }

    #[test]
    fn parses_lists_json_and_truncated_blocks() {
        let a = "<CARD>\ndone: no\nopen issues:\n- leap years\n- negative input\nartifacts:\n- src/a.rs\n";
        let c = parse_block(a).unwrap();
        assert!(!c.done);
        assert_eq!(c.open_issues, vec!["leap years", "negative input"]);
        assert_eq!(c.artifacts, vec!["src/a.rs"]);
        let j = r#"<card>{"done": true, "summary": "s", "evidence": ["e1"], "artifacts": "x.py"}</card>"#;
        let c = parse_block(j).unwrap();
        assert!(c.done && c.evidence == vec!["e1"] && c.artifacts == vec!["x.py"]);
        assert!(parse_block("no card here").is_none());
        assert!(parse_block("<card>\njust words\n</card>").is_none());
    }

    #[test]
    fn extracts_a_card_from_free_text() {
        let a = "Fixed the parser in src/date.rs and added a test.\nRan cargo test: 12 passed.\n\
                 TODO: week 53 is not handled yet.\nCommitted as commit 9f8e7d6c, PR \
                 https://github.com/o/r/pull/12\n```rust\nfn x() {} // not.rs\n```";
        let c = extract(a);
        assert_eq!(c.source, "extracted");
        assert!(!c.done, "'not ... yet' is an open issue: {c:?}");
        assert!(c.summary.starts_with("Fixed the parser"));
        assert!(c.evidence.iter().any(|e| e.contains("12 passed")));
        assert!(c.open_issues.iter().any(|e| e.contains("week 53")));
        assert!(c
            .artifacts
            .contains(&"https://github.com/o/r/pull/12".to_string()));
        assert!(c.artifacts.contains(&"commit 9f8e7d6c".to_string()));
        assert!(c.artifacts.contains(&"src/date.rs".to_string()));
        assert!(
            !c.artifacts.iter().any(|x| x.contains("not.rs")),
            "code is not prose"
        );
        assert!(extract("Implemented it; all tests pass.").done);
        assert!(!extract("I could not finish.").done);
    }

    #[test]
    fn from_answer_prefers_the_block() {
        assert_eq!(from_answer("x\n<card>\ndone: yes\n</card>").source, "block");
        assert_eq!(from_answer("did it").source, "extracted");
        assert_eq!(est_tokens("abcde"), 2);
    }
}

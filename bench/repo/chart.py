#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Render docs/media/results-repo.svg from bench/repo/results.json (the README chart).

One block per player: one bar per arm, length = wall time (shorter is better, scaled per player
so every block reads on its own), labelled with verified tasks and billable tokens / cost.
Standard library only.

    python3 bench/repo/chart.py
"""
import json, os

HERE = os.path.dirname(os.path.abspath(__file__))
OUT = os.path.join(HERE, "..", "..", "docs", "media", "results-repo.svg")
res = json.load(open(os.path.join(HERE, "results.json")))["players"]

# The chart shows one clean row per setup, in this order (repeat runs and superseded runs
# stay in results.json and results.md).
SHOW = [
    ("haiku", "Claude Haiku · Claude Code"),
    ("codex cli (gpt-6-luna low)", "GPT Luna (low) · Codex CLI"),
    ("nanocode (gpt-6-luna low)", "GPT Luna (low) · nanocode"),
    ("luna (OpenUnum agent loop) r3 cache-key", "GPT Luna (low) · OpenUnum agent loop"),
    ("qwen", "local Qwen3.8 Flash Next · OpenUnum"),
    ("oc:nvidia/nemotron-3-ultra-550b-a55b", "Nemotron 3 Ultra · NVIDIA NIM (free) · opencode"),
    ("oc:nemotron-3-ultra-free", "Nemotron 3 Ultra · opencode free tier"),
    ("oc:openai/gpt-oss-20b", "gpt-oss-20b · NVIDIA NIM (free) · opencode"),
]
NAMES = dict(SHOW)
ARMS = [("A", "one agent, all tasks", "#7c8db5"), ("B", "UnumPingPong tables", "#30a46c"),
        ("C", "tables, shared session", "#3e63dd")]


def name(k):
    if k in NAMES:
        return NAMES[k]
    if k.startswith("oc:"):
        return k[3:] + " · opencode"
    return k


def esc(s):
    return s.replace("&", "&amp;").replace("<", "&lt;").replace(">", "&gt;")


def fmt_tokens(c):
    t = c["tokens"]["billable"]
    if not t:
        return ""
    return f"{t / 1000:.0f}k tokens" + (f" · ${c['cost_usd']:.3f}" if c.get("cost_usd") else "")


# Players with at least one complete arm; local-model runs with 0 tokens still show time.
rows = [(k, res[k]) for k, _ in SHOW if k in res and any(a in res[k] for a, _, _ in ARMS)]
W, X0, BAR_MAX = 900, 250, 290
y = 96
parts = []
for k, arms in rows:
    present = [(a, lab, col) for a, lab, col in ARMS if a in arms]
    tmax = max(arms[a]["wall_s"] for a, _, _ in present) or 1
    parts.append(f'<text x="40" y="{y + 14}" class="p">{esc(name(k))}</text>')
    for i, (a, lab, col) in enumerate(present):
        c = arms[a]
        yy = y + 26 + i * 26
        w = max(6, BAR_MAX * c["wall_s"] / tmax)
        ok = c["points"] == c["tasks"]
        parts.append(f'<text x="40" y="{yy + 13}" class="a">{a} · {lab}</text>')
        parts.append(f'<rect x="{X0}" y="{yy}" width="{w:.0f}" height="18" rx="4" fill="{col}">'
                     f'<animate attributeName="width" from="0" to="{w:.0f}" dur="1.2s" begin="{0.2 + i * 0.2:.1f}s" fill="freeze"/></rect>')
        label = f'{c["wall_s"] / 60:.1f} min · {c["points"]}/{c["tasks"]} ✓'
        extra = fmt_tokens(c)
        parts.append(f'<text x="{X0 + w + 10:.0f}" y="{yy + 13}" class="v" style="fill:{"#e8edf7" if ok else "#ffb4b6"}">'
                     f'{esc(label)}{(" · " + esc(extra)) if extra else ""}</text>')
    y += 26 + 26 * len(present) + 22
H = y + 30
svg = f'''<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {W} {H}" width="{W}" height="{H}" font-family="ui-sans-serif, -apple-system, Segoe UI, Helvetica, Arial, sans-serif">
  <title>Real-repository benchmark: 10 real bug fixes, one agent session vs UnumPingPong tables, per player</title>
  <style>
    .h {{ font-size: 18px; font-weight: 800; fill: #e8edf7; }}
    .s {{ font-size: 12px; fill: #93a1bd; }}
    .p {{ font-size: 14px; font-weight: 700; fill: #e8edf7; }}
    .a {{ font-size: 12px; fill: #93a1bd; }}
    .v {{ font-size: 12px; font-weight: 600; }}
  </style>
  <rect width="{W}" height="{H}" rx="18" fill="#0b1020"/>
  <text x="40" y="44" class="h">10 real bug fixes, real tests, same tasks for everyone</text>
  <text x="40" y="66" class="s">bar = wall time, shorter is better (scaled per player) · ✓ = tasks whose real unit test passes afterwards</text>
  {chr(10).join("  " + p for p in parts)}
  <text x="40" y="{H - 18}" class="s">source: bench/repo/results.json · tasks: bench/repo/tasks.json · one run per cell</text>
</svg>
'''
open(OUT, "w").write(svg)
print("wrote", os.path.relpath(OUT), len(rows), "players")

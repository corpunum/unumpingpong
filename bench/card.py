#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Render docs/media/pingpong-result.png from bench/results.json (needs Pillow).

One panel per task set; the bars are the headline metrics (verified tasks per minute and per
100k tokens) plus verified tasks, wall time and tokens, for every arm that ran."""
import json, os
from PIL import Image, ImageDraw, ImageFont

H = os.path.dirname(os.path.abspath(__file__))
r = json.load(open(os.path.join(H, "results.json")))
F = "/usr/share/fonts/truetype/dejavu/DejaVuSansMono"
f = lambda s, b=False: ImageFont.truetype(F + ("-Bold" if b else "") + ".ttf", s)
BG, FG, DIM, BAR = "#1e2030", "#e6e8f0", "#8b90a8", "#3a3f5c"
COL = {"A": "#f5d76e", "B": "#6ee7a0", "C": "#7cc7ff",
       "haiku:A": "#ffb36b", "haiku:B": "#ff7a59", "luna-low:A": "#c9a7ff", "luna-low:B": "#a05cff"}
NAME = {"A": "Qwen local: A one agent loop", "B": "Qwen local: B tables", "C": "Qwen local: C tables + orchestrator",
        "haiku:A": "Haiku: A one session", "haiku:B": "Haiku: B tables",
        "luna-low:A": "Luna low: A one session", "luna-low:B": "Luna low: B tables"}
sets = [(k, v) for k, v in r["sets"].items() if any(a in v for a in "BC")]
# Cloud players (Claude Code / Codex CLIs) join the same panels as extra series, keyed "player:arm".
for _, v in sets:
    for lab, arms in v.get("cloud", {}).items():
        for arm, x in arms.items():
            v[f"{lab}:{arm}"] = x
W = 1200
rows = [("verified tasks", lambda x: x["points"], lambda x: f"{x['points']}/{x['tasks']}"),
        ("verified / minute", lambda x: x["verified_per_min"], lambda x: f"{x['verified_per_min']:.2f}"),
        ("verified / 100k tokens", lambda x: x["verified_per_100k_tokens"], lambda x: f"{x['verified_per_100k_tokens']:.1f}"),
        ("wall time", lambda x: x["wall_s"], lambda x: f"{x['wall_s']:.0f} s"),
        ("tokens", lambda x: x["tokens"], lambda x: f"{x['tokens']:,}")]
ORDER = ["A", "B", "C", "haiku:A", "haiku:B", "luna-low:A", "luna-low:B"]
arms_of = lambda s: [a for a in ORDER if a in s]
bar_sets = [(k, v) for k, v in sets if len(arms_of(v)) >= 2]
text_sets = [(k, v) for k, v in sets if len(arms_of(v)) < 2]
Hh = 190 + sum(60 + len(rows) * (14 + 26 * len(arms_of(s))) for _, s in bar_sets) + 150 * len(text_sets) + 50
im = Image.new("RGB", (W, Hh), BG); d = ImageDraw.Draw(im)
d.text((40, 26), "UnumPingPong vs a single agent loop", font=f(30, True), fill=FG)
d.text((40, 70), f"local Qwen (reference) vs Claude Haiku and Luna-low CLI players; unit tests as the referee, one run per arm",
       font=f(16), fill=DIM)
x = 40; ly = 100
for a in ORDER:
    if any(a in s for _, s in sets):
        if x > 900:
            x = 40; ly += 24
        d.rectangle((x, ly + 4, x + 14, ly + 18), fill=COL[a]); d.text((x + 20, ly), NAME[a], font=f(14), fill=FG); x += 290
y = ly + 40
for name, s in bar_sets:
    arms = arms_of(s)
    when = "runs " + ", ".join(sorted({s[a].get("date", "") for a in arms}))
    d.text((40, y), f"{name} set: {len(s['tasks'])} tasks", font=f(20, True), fill=FG)
    d.text((330, y + 4), when, font=f(13), fill=DIM); y += 40
    for label, val, txt in rows:
        d.text((40, y + 2), label, font=f(15), fill=DIM)
        m = max((val(s[a]) or 0) for a in arms) or 1
        for a in arms:
            v = val(s[a]) or 0
            d.rounded_rectangle((300, y, 1160, y + 22), 4, fill=BAR)
            bw = max(6, int(860 * v / m))
            d.rounded_rectangle((300, y, 300 + bw, y + 22), 4, fill=COL[a])
            t_ = txt(s[a])
            if bw > 12 * len(t_) + 16:
                d.text((308, y + 2), t_, font=f(15, True), fill="#101018")
            else:
                d.text((300 + bw + 8, y + 2), t_, font=f(15, True), fill=FG)
            y += 26
        y += 14
    y += 6
for name, s in text_sets:
    a = arms_of(s)[0]; x_ = s[a]
    d.text((40, y), f"{name} set: {len(s['tasks'])} tasks ({NAME[a]} only, partial)", font=f(20, True), fill=FG); y += 36
    lines = [f"{x_['points']}/{x_['tasks']} verified in {x_['wall_s'] / 60:.0f} min ({x_['tokens']:,} tokens) before the run was capped",
             f"the referee rejected {x_.get('player_claimed_done_referee_failed', '?')} of "
             f"{x_.get('block_cards_claiming_done', '?')} 'done' claims in the player's own cards",
             f"{x_.get('rallies_at_max_tokens', '?')} of {x_['rallies']} rallies used the whole token budget; no A run yet"]
    for l in lines:
        d.text((60, y), "- " + l, font=f(16), fill=FG); y += 26
    y += 20
d.text((40, Hh - 40), "Wall times differ in endpoint load between runs; details and caveats: bench/results.md",
       font=f(14), fill=DIM)
im.save(os.path.join(H, "..", "docs", "media", "pingpong-result.png"))
print("wrote docs/media/pingpong-result.png", W, Hh)

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
COL = {"A": "#f5d76e", "B": "#6ee7a0", "C": "#7cc7ff"}
NAME = {"A": "A one agent loop", "B": "B tables", "C": "C tables + orchestrator"}
sets = [(k, v) for k, v in r["sets"].items() if any(a in v for a in "BC")]
W = 1200
rows = [("verified tasks", lambda x: x["points"], lambda x: f"{x['points']}/{x['tasks']}"),
        ("verified / minute", lambda x: x["verified_per_min"], lambda x: f"{x['verified_per_min']:.2f}"),
        ("verified / 100k tokens", lambda x: x["verified_per_100k_tokens"], lambda x: f"{x['verified_per_100k_tokens']:.1f}"),
        ("wall time", lambda x: x["wall_s"], lambda x: f"{x['wall_s']:.0f} s"),
        ("tokens", lambda x: x["tokens"], lambda x: f"{x['tokens']:,}")]
arms_of = lambda s: [a for a in "ABC" if a in s]
bar_sets = [(k, v) for k, v in sets if len(arms_of(v)) >= 2]
text_sets = [(k, v) for k, v in sets if len(arms_of(v)) < 2]
Hh = 150 + sum(60 + len(rows) * (14 + 26 * len(arms_of(s))) for _, s in bar_sets) + 150 * len(text_sets) + 50
im = Image.new("RGB", (W, Hh), BG); d = ImageDraw.Draw(im)
d.text((40, 26), "UnumPingPong vs a single agent loop", font=f(30, True), fill=FG)
d.text((40, 70), f"same local model ({r['model'].split('/')[-1]}), unit tests as the referee, one run per arm",
       font=f(16), fill=DIM)
x = 40
for a in "ABC":
    if any(a in s for _, s in sets):
        d.rectangle((x, 104, x + 14, 118), fill=COL[a]); d.text((x + 20, 100), NAME[a], font=f(15), fill=FG); x += 260
y = 140
for name, s in bar_sets:
    arms = arms_of(s)
    when = "; ".join(f"{a}: {s[a].get('date', '')}" for a in arms)
    d.text((40, y), f"{name} set: {len(s['tasks'])} tasks", font=f(20, True), fill=FG)
    d.text((330, y + 4), when, font=f(13), fill=DIM); y += 40
    for label, val, txt in rows:
        d.text((40, y + 2), label, font=f(15), fill=DIM)
        m = max((val(s[a]) or 0) for a in arms) or 1
        for a in arms:
            v = val(s[a]) or 0
            d.rounded_rectangle((300, y, 1160, y + 22), 4, fill=BAR)
            d.rounded_rectangle((300, y, 300 + max(6, int(860 * v / m)), y + 22), 4, fill=COL[a])
            d.text((308, y + 2), txt(s[a]), font=f(15, True), fill="#101018")
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

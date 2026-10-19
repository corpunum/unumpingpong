#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Render docs/media/pingpong-result.png from bench/results.json (needs Pillow)."""
import json, os
from PIL import Image, ImageDraw, ImageFont
H = os.path.dirname(os.path.abspath(__file__))
r = json.load(open(os.path.join(H, "results.json"))); A, B = r["A"], r["B"]; n = A["tasks"]
F = "/usr/share/fonts/truetype/dejavu/DejaVuSansMono"
f = lambda s, b=False: ImageFont.truetype(F + ("-Bold" if b else "") + ".ttf", s)
BG, FG, DIM, RED, GRN, YEL, BAR = "#1e2030", "#e6e8f0", "#8b90a8", "#ff6b6b", "#6ee7a0", "#f5d76e", "#3a3f5c"
W, Hh = 1200, 610
im = Image.new("RGB", (W, Hh), BG); d = ImageDraw.Draw(im)
d.text((40, 30), "UnumPingPong vs a single agent loop", font=f(30, True), fill=FG)
d.text((40, 74), f"{n} small coding tasks, unit tests as referee, one local model ({r['model'].split('/')[-1]}), {r['date']}", font=f(16), fill=DIM)
rows = [("points (referee-verified)", A["points"], B["points"], f"{A['points']}/{n}", f"{B['points']}/{n}", True),
        ("tokens (prompt + completion)", A["tokens"], B["tokens"], f"{A['tokens']:,}", f"{B['tokens']:,}", False),
        ("wall time", A["wall_s"], B["wall_s"], f"{A['wall_s']:.0f} s", f"{B['wall_s']:.0f} s", False),
        ("model calls / rallies", A["turns"], B["rallies"], f"{A['turns']}", f"{B['rallies']}", False)]
y = 130
d.text((40, y), "A  one agent loop (model says DONE)", font=f(16, True), fill=YEL)
d.text((640, y), "B  UnumPingPong tables (cap 3, referee per rally)", font=f(16, True), fill=GRN)
y += 40
for label, va, vb, ta, tb, higher_better in rows:
    d.text((40, y), label, font=f(18), fill=FG); y += 30
    m = max(va, vb) or 1
    for x0, v, t, col in ((40, va, ta, YEL), (640, vb, tb, GRN)):
        d.rounded_rectangle((x0, y, x0 + 520, y + 26), 5, fill=BAR)
        d.rounded_rectangle((x0, y, x0 + max(8, int(520 * v / m)), y + 26), 5, fill=col)
        d.text((x0 + 8, y + 3), t, font=f(18, True), fill="#101018")
    y += 52
ratio = A["tokens"] / B["tokens"]
claimed = "no false claim" if not A["claimed_done_but_failed"] else f"claimed done with {A['failed_when_claimed_done']} failing"
d.text((40, y + 4), f"B used {ratio:.1f}x fewer tokens ({B['prompt_tokens']:,} vs {A['prompt_tokens']:,} prompt tokens), but took "
       f"{B['wall_s'] / A['wall_s']:.1f}x the wall time.", font=f(17, True), fill=FG)
d.text((40, y + 36), f"Both solved {A['points']}/{n}; A's \"done\" was {claimed} on this easy set, so the referee changed nothing here.", font=f(16), fill=DIM)
d.text((40, y + 62), "One run per arm on a shared endpoint; see bench/results.md. Reproduce: python3 bench/bench.py", font=f(14), fill=DIM)
im.save(os.path.join(H, "..", "docs", "media", "pingpong-result.png"))

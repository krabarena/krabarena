#!/usr/bin/env python3
"""Regenerate the X (Twitter) social assets.

Requires Pillow. Outputs `static/social/x-profile.png` (400x400) and
`static/social/x-banner.png` (1500x500), both deterministic from the
existing `apple-touch-icon.png` crab + brand colors + bundled Inter fonts.
"""
from PIL import Image, ImageDraw, ImageFont
import os

CRATE = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
STATIC = os.path.join(CRATE, 'static')
OUT = os.path.join(STATIC, 'social')
os.makedirs(OUT, exist_ok=True)

CRAB = Image.open(os.path.join(STATIC, 'apple-touch-icon.png')).convert('RGBA')
BRAND = (240, 104, 39)
INK   = (17, 17, 17)
MUTED = (96, 102, 110)
CREAM = (252, 247, 240)

F_BLACK = os.path.join(STATIC, 'fonts/Inter-Black.ttf')
F_BOLD  = os.path.join(STATIC, 'fonts/Inter-Bold.ttf')
F_REG   = os.path.join(STATIC, 'fonts/Inter-Regular.ttf')

# Profile 400x400
PROF = 400
prof = Image.new('RGBA', (PROF, PROF), CREAM + (255,))
ring = Image.new('RGBA', (PROF, PROF), (0,0,0,0))
ImageDraw.Draw(ring).ellipse([(8,8),(PROF-8,PROF-8)], outline=BRAND+(255,), width=6)
prof.alpha_composite(ring)
cz = 280
prof.alpha_composite(CRAB.resize((cz,cz), Image.LANCZOS), dest=((PROF-cz)//2,(PROF-cz)//2))
prof.save(os.path.join(OUT, 'x-profile.png'), optimize=True)

# Banner 1500x500.
#
# Layout note: X overlays the round profile picture at ~bottom-left
# of the banner (avatar diameter ~200px, ~133px in from the left
# edge). The previous version painted a 320x320 crab on the orange
# wedge in that exact zone, which stacked behind the avatar's own
# crab — "crab on crab" visual duplication. Removed the wedge crab;
# the orange wedge alone is the brand accent, and the round avatar
# carries the only crab.
BW, BH = 1500, 500
banner = Image.new('RGBA', (BW, BH), CREAM + (255,))
b = ImageDraw.Draw(banner)
b.polygon([(0,0),(380,0),(280,BH),(0,BH)], fill=BRAND+(255,))
T = ImageFont.truetype(F_BLACK, 110)
S = ImageFont.truetype(F_BOLD, 36)
D = ImageFont.truetype(F_REG, 30)
TX = 440
b.text((TX, 145), 'KrabArena', font=T, fill=INK+(255,))
b.text((TX, 270), 'Distributed benchmarking for the AI era.', font=S, fill=BRAND+(255,))
b.text((TX, 322), 'Agents publish runnable Claims. Humans & agents Verify or Refute.', font=D, fill=MUTED+(255,))
b.text((TX, 362), 'No takes, no upvote-as-truth — only proof of work.', font=D, fill=MUTED+(255,))
b.text((TX, 420), 'krabarena.com', font=S, fill=INK+(255,))
banner.save(os.path.join(OUT, 'x-banner.png'), optimize=True)
print('wrote', os.path.join(OUT, 'x-profile.png'))
print('wrote', os.path.join(OUT, 'x-banner.png'))

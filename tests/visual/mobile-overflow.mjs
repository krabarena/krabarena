// Mobile layout regression test + screenshot capture.
//
// Guards the 0.21.3 fix: on phones the page must NOT overflow
// horizontally and status pills must not be clipped ("active" → "act").
// `body{overflow-x:clip}` hides page scroll, so a plain scrollbar check
// is not enough — we also flag any element whose text is clipped by a
// non-scrollable overflow.
//
// For each page × width it (1) asserts no horizontal page overflow and
// no clipped pills, and (2) writes a screenshot to ./screenshots/ so the
// committed set is a visual reference (git diffs surface layout changes).
//
// Run:
//   cd tests/visual && npm install && npm test
//   BASE_URL=https://krabarena.com npm test         # against prod
//   BASE_URL=http://127.0.0.1:3013 npm test          # local server
//
// Exit code is non-zero if any page overflows or clips — usable as a gate.

import { chromium } from 'playwright';
import { mkdirSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

const BASE = process.env.BASE_URL || 'http://127.0.0.1:3013';
const WIDTHS = [320, 360, 390];
const HERE = dirname(fileURLToPath(import.meta.url));
const SHOTS = join(HERE, 'screenshots');
mkdirSync(SHOTS, { recursive: true });

// Elements whose horizontal clipping is intentional (ellipsis truncation).
const IGNORE = /\b(name|cell-clamp|th-label|ellipsis)\b/;

async function discoverBattleSlug() {
  const res = await fetch(`${BASE}/`, { headers: { Accept: 'text/markdown' } });
  const md = await res.text();
  const m = md.match(/\/battles\/([a-z0-9-]+)/i);
  return m ? `/battles/${m[1]}` : null;
}

function audit() {
  const vw = document.documentElement.clientWidth;
  const pageOverflow = document.body.scrollWidth - vw;
  const clipped = [];
  for (const el of document.querySelectorAll('*')) {
    const cs = getComputedStyle(el);
    if (cs.overflowX === 'visible' &&
        el.scrollWidth > el.clientWidth + 2 && el.clientWidth > 40) {
      const cls = (el.className || '').toString();
      if (!/\b(name|cell-clamp|th-label|ellipsis)\b/.test(cls)) {
        clipped.push(`${el.tagName.toLowerCase()}.${cls.slice(0, 30)} (${el.scrollWidth}>${el.clientWidth})`);
      }
    }
  }
  return { vw, pageOverflow, clipped: [...new Set(clipped)] };
}

const browser = await chromium.launch();
let failures = 0;
const battle = await discoverBattleSlug();
const pages = [
  ['home', '/'],
  ...(battle ? [['battle', battle]] : []),
  ['arena', '/arena'],
  ['agents', '/agents'],
];

for (const w of WIDTHS) {
  const ctx = await browser.newContext({ viewport: { width: w, height: 800 } });
  const page = await ctx.newPage();
  for (const [name, path] of pages) {
    await page.goto(`${BASE}${path}`, { waitUntil: 'networkidle' });
    const r = await page.evaluate(audit);
    await page.screenshot({ path: join(SHOTS, `${name}-${w}.png`), fullPage: false });
    const bad = r.pageOverflow > 0 || r.clipped.length > 0;
    if (bad) failures++;
    console.log(`${bad ? 'FAIL' : 'ok  '}  ${name} @${w}  pageOverflow=${r.pageOverflow}  clipped=${r.clipped.length}` +
      (r.clipped.length ? `\n        ${r.clipped.join('\n        ')}` : ''));
  }
  await ctx.close();
}
await browser.close();

if (failures) { console.error(`\n${failures} mobile-overflow failure(s).`); process.exit(1); }
console.log('\nAll pages fit at 320/360/390px — no horizontal overflow or clipped content.');

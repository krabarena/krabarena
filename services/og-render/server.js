'use strict';

// KrabArena OG-card render microservice.
//
// Screenshots the krabarena-api `/og-html/...` leaderboard templates
// into 1200x630 PNGs with a single, warm headless-Chromium instance.
// Keeping Chromium here (and not in the Rust API container) is the
// whole point: the API stays a slim axum binary, and this service owns
// the heavy browser.
//
// Routes:
//   GET /health           -> {status, browser} liveness probe.
//   GET /render?path=/x    -> navigate to KRAB_HTML_BASE + path,
//                             screenshot, return image/png.
//   GET /render?url=https… -> navigate to a full absolute URL instead
//                             (must be http/https; used for local
//                             smoke tests against a throwaway page).
//
// Env:
//   PORT            listen port (default 8080).
//   KRAB_HTML_BASE  base URL of the krabarena-api serving /og-html
//                   (e.g. https://krabarena.com or the test Cloud Run
//                   URL). Required for the `path=` form.
//   RENDER_TIMEOUT_MS  per-render navigation/screenshot budget
//                   (default 8000). The API caller uses its own tight
//                   ~5s timeout on top of this.

const express = require('express');
const { chromium } = require('playwright');

const PORT = parseInt(process.env.PORT || '8080', 10);
const HTML_BASE = (process.env.KRAB_HTML_BASE || '').replace(/\/+$/, '');
const RENDER_TIMEOUT_MS = parseInt(process.env.RENDER_TIMEOUT_MS || '8000', 10);

const VIEWPORT = { width: 1200, height: 630 };

// One browser, reused across every request. Launched lazily on the
// first render (and re-launched if it ever disconnects) so the
// container can start + answer /health before Chromium is warm.
let browserPromise = null;

async function getBrowser() {
  if (browserPromise) {
    const b = await browserPromise;
    if (b.isConnected()) return b;
    // Stale handle (crashed / disconnected) — drop it and relaunch.
    browserPromise = null;
  }
  browserPromise = chromium.launch({
    headless: true,
    args: [
      '--no-sandbox',
      '--disable-dev-shm-usage',
      '--disable-gpu',
      '--font-render-hinting=none',
    ],
  });
  return browserPromise;
}

// Resolve the target URL from the query. `url=` wins (absolute,
// validated http/https); otherwise `path=` is appended to
// KRAB_HTML_BASE. Throws a string error for the caller to 4xx.
function resolveTarget(query) {
  if (query.url) {
    let parsed;
    try {
      parsed = new URL(query.url);
    } catch (_e) {
      throw 'invalid url= parameter';
    }
    // http/https for real cards. file: only when OG_RENDER_ALLOW_FILE_URLS=1
    // (set by local-test.sh) — otherwise `url=file:///proc/self/environ`
    // would screenshot the container's environment.
    const allowFile = process.env.OG_RENDER_ALLOW_FILE_URLS === '1';
    if (
      parsed.protocol !== 'http:' &&
      parsed.protocol !== 'https:' &&
      !(allowFile && parsed.protocol === 'file:')
    ) {
      throw allowFile ? 'url= must be http, https, or file' : 'url= must be http or https';
    }
    return parsed.toString();
  }
  if (query.path) {
    if (!HTML_BASE) {
      throw 'KRAB_HTML_BASE not configured (required for path=)';
    }
    const path = query.path.startsWith('/') ? query.path : `/${query.path}`;
    return `${HTML_BASE}${path}`;
  }
  throw 'missing path= or url= query parameter';
}

async function renderPng(target) {
  const browser = await getBrowser();
  const context = await browser.newContext({
    viewport: VIEWPORT,
    deviceScaleFactor: 1,
  });
  const page = await context.newPage();
  try {
    // The browser is reused across renders, so its HTTP cache can outlive a
    // deploy and keep serving the /og-html HTML/CSS/fonts from the OLD card
    // design. Disable the cache per page so every render fetches the current
    // templates + assets. (deploy.yml also restarts og-render each deploy as
    // belt-and-braces; this makes a stale render impossible even without it.)
    const cdp = await context.newCDPSession(page);
    await cdp.send('Network.setCacheDisabled', { cacheDisabled: true });
    await page.goto(target, {
      waitUntil: 'networkidle',
      timeout: RENDER_TIMEOUT_MS,
    });
    // Belt-and-braces: make sure web fonts are actually applied before
    // we snap, so the screenshot isn't a fallback-font flash.
    await page
      .evaluate(() => (document.fonts ? document.fonts.ready : null))
      .catch(() => {});
    const buf = await page.screenshot({
      type: 'png',
      clip: { x: 0, y: 0, width: VIEWPORT.width, height: VIEWPORT.height },
    });
    return buf;
  } finally {
    // Close the context (and its page); keep the browser warm.
    await context.close().catch(() => {});
  }
}

const app = express();

app.get('/health', async (_req, res) => {
  let browserOk = false;
  if (browserPromise) {
    try {
      const b = await browserPromise;
      browserOk = b.isConnected();
    } catch (_e) {
      browserOk = false;
    }
  }
  res.json({
    status: 'ok',
    browser: browserOk ? 'connected' : 'cold',
    html_base: HTML_BASE || null,
  });
});

app.get('/render', async (req, res) => {
  let target;
  try {
    target = resolveTarget(req.query);
  } catch (msg) {
    res.status(400).type('text/plain').send(String(msg));
    return;
  }
  try {
    const png = await renderPng(target);
    res
      .status(200)
      .type('image/png')
      .set('Cache-Control', 'public, max-age=300')
      .send(png);
  } catch (err) {
    console.error(`render failed for ${target}:`, err && err.message ? err.message : err);
    res.status(502).type('text/plain').send('render failed');
  }
});

const server = app.listen(PORT, () => {
  console.log(`og-render listening on :${PORT} (html_base=${HTML_BASE || 'unset'})`);
});

// Warm the browser in the background so the first real /render is fast.
getBrowser().catch((e) => console.error('browser warm-up failed:', e && e.message));

function shutdown(sig) {
  console.log(`received ${sig}, shutting down`);
  server.close(() => {
    if (browserPromise) {
      browserPromise
        .then((b) => b.close())
        .catch(() => {})
        .finally(() => process.exit(0));
    } else {
      process.exit(0);
    }
  });
  // Hard exit if graceful close hangs.
  setTimeout(() => process.exit(0), 5000).unref();
}
process.on('SIGTERM', () => shutdown('SIGTERM'));
process.on('SIGINT', () => shutdown('SIGINT'));

import { access, readdir, readFile } from "node:fs/promises";
import { relative, resolve } from "node:path";

const templatesDir = resolve(import.meta.dirname, "../../templates");
// Hand-written HTML served by the app from /static (e.g. /static/demo/).
// The CSP (`web::security_headers`) applies to these too and they get no
// nonce, so they must not carry any inline script or handler.
const staticDir = resolve(import.meta.dirname, "../../static");

const INLINE_SCRIPT_ALLOWLIST = new Set([]);
const violations = [];

try {
  await access(templatesDir);
} catch {
  throw new Error(`Template directory not found: ${templatesDir}`);
}

for (const path of await htmlFiles(templatesDir)) {
  const file = relative(templatesDir, path);
  const source = await readFile(path, "utf8");
  checkNoInlineHandlers(file, source);
  checkNoJavascriptUrls(file, source);
  checkInlineScriptsCarryNonce(file, source);
  checkNoLegacyScripts(file, source);
  checkNoDirectLocalStorage(file, source);
  checkNoTemporaryTemplateClasses(file, source);
  checkNoRawUiSvg(file, source);
  checkNoInlineScripts(file, source);
  checkNoInlineStyleTags(file, source);
  checkNoUnsafeStyleAttrs(file, source);
}

// script-src has no 'unsafe-inline': an inline handler, javascript: URL
// or inline <script> in static HTML would be blocked by the CSP.
for (const path of await htmlFiles(staticDir, ["dist"])) {
  const file = `static/${relative(staticDir, path)}`;
  const source = await readFile(path, "utf8");
  checkNoInlineHandlers(file, source);
  checkNoJavascriptUrls(file, source);
  checkNoInlineScriptsAtAll(file, source);
}

if (violations.length) {
  console.error(
    ["Template lint failed:", ...violations.map((violation) => `- ${violation}`)].join("\n")
  );
  process.exitCode = 1;
}

// Any `on<event>=` attribute inside a tag. The CSP (no 'unsafe-inline' in
// script-src, `script-src-attr 'none'`) blocks every one of them, so
// behaviour must be wired with addEventListener in frontend/src.
function checkNoInlineHandlers(file, source) {
  const matches = source.match(/<[a-z][^>]*\son[a-z]+\s*=/gi) || [];
  if (matches.length) {
    violations.push(`${file}: inline event handlers are not allowed (${matches.length} found)`);
  }
}

function checkNoJavascriptUrls(file, source) {
  const matches =
    source.match(/\b(?:href|src|action|formaction)\s*=\s*["']?\s*javascript:/gi) || [];
  if (matches.length) {
    violations.push(`${file}: javascript: URLs are blocked by the CSP (${matches.length} found)`);
  }
}

// Non-executed data blocks (JSON-LD) are the only inline scripts allowed
// in templates; they still carry the per-response CSP nonce so any future
// executable inline script copies a working pattern.
function checkInlineScriptsCarryNonce(file, source) {
  const scripts = [...source.matchAll(/<script\b([^>]*)>/gi)];
  scripts.forEach((match) => {
    const attrs = match[1] || "";
    if (/\ssrc\s*=/.test(attrs)) return;
    if (!/\snonce="\{\{ meta\.csp_nonce \}\}"/.test(attrs)) {
      violations.push(`${file}: inline <script> must carry nonce="{{ meta.csp_nonce }}"`);
    }
  });
}

function checkNoInlineScriptsAtAll(file, source) {
  const scripts = [...source.matchAll(/<script\b([^>]*)>/gi)];
  scripts.forEach((match) => {
    if (!/\ssrc\s*=/.test(match[1] || "")) {
      violations.push(`${file}: inline <script> is not allowed in static HTML (no CSP nonce)`);
    }
  });
}

function checkNoLegacyScripts(file, source) {
  const legacy = ["/static/analytics.js", "/static/sort.js", "/static/sortable-tables.js"];
  legacy.forEach((script) => {
    if (source.includes(script)) {
      violations.push(`${file}: legacy script reference ${script} is not allowed`);
    }
  });
}

function checkNoDirectLocalStorage(file, source) {
  if (source.includes("localStorage")) {
    violations.push(`${file}: direct localStorage usage belongs in frontend/src/session.js`);
  }
}

function checkNoTemporaryTemplateClasses(file, source) {
  const matches = source.match(/u-tpl-[a-f0-9]+/gi) || [];
  if (matches.length) {
    violations.push(
      `${file}: temporary u-tpl-* classes are not allowed; use CUBE classes (${matches.length} found)`
    );
  }
}

function checkNoRawUiSvg(file, source) {
  if (source.includes("<svg")) {
    violations.push(
      `${file}: raw UI <svg> is not allowed; use external SVG assets or PageMeta icon helpers`
    );
  }
}

function checkNoInlineScripts(file, source) {
  if (INLINE_SCRIPT_ALLOWLIST.has(file)) return;
  const scripts = [...source.matchAll(/<script\b([^>]*)>/gi)];
  scripts.forEach((match) => {
    const attrs = match[1] || "";
    const hasSrc = /\ssrc\s*=/.test(attrs);
    const isJsonLd = /\stype\s*=\s*["']application\/ld\+json["']/i.test(attrs);
    if (!hasSrc && !isJsonLd) {
      violations.push(`${file}: inline <script> is not allowed`);
    }
  });
}

function checkNoInlineStyleTags(file, source) {
  if (/<style\b/i.test(source)) {
    violations.push(`${file}: inline <style> is not allowed; move CSS to frontend/src/styles`);
  }
}

function checkNoUnsafeStyleAttrs(file, source) {
  const styles = [...source.matchAll(/\bstyle\s*=\s*(?:"[^"]*"|'[^']*'|[^\s>]+)/gi)];
  if (styles.length) {
    violations.push(
      `${file}: inline style attributes are not allowed; use CUBE classes or data-* hydration (${styles.length} found)`
    );
  }
}

async function htmlFiles(dir, skipDirs = []) {
  const entries = await readdir(dir, { withFileTypes: true });
  const files = await Promise.all(
    entries.map(async (entry) => {
      const path = resolve(dir, entry.name);
      if (entry.isDirectory()) {
        return skipDirs.includes(entry.name) ? [] : htmlFiles(path, skipDirs);
      }
      if (entry.isFile() && entry.name.endsWith(".html")) return [path];
      return [];
    })
  );
  return files.flat();
}

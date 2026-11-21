const COLOR_RE = /^(#[0-9a-f]{3}(?:[0-9a-f]{3})?|var\(--[a-z0-9-]+\))$/i;
const rules = new Set();
let styleSheet = null;

export function initDynamicStyles() {
  applyNumberProperty("[data-verify-flex]", "verifyFlex", "--verify-flex");
  applyNumberProperty("[data-refute-flex]", "refuteFlex", "--refute-flex");

  applyColorProperty("[data-logo-bg]", "logoBg", "--logo-bg");
  applyColorProperty("[data-logo-fg]", "logoFg", "--logo-fg");
  applyColorProperty("[data-wins-bg]", "winsBg", "--wins-bg");
  applyWinsPieGradients();

  // Claim hero modules (race bars / frontier scatter dots) — CSP-safe
  // positioning from data attributes, same pattern as the properties
  // above. Percentages are clamped to 0..100 so a malformed value
  // (or a future geometry-formula bug) can't push a mark off-card.
  applyNumberProperty("[data-bar-width]", "barWidth", "width", "%", 0, 100);
  // Crawler Zoo vertical bar charts (/zoo, /zoo/bots/*).
  applyNumberProperty("[data-bar-height]", "barHeight", "height", "%", 0, 100);
  applyNumberProperty("[data-x]", "x", "left", "%", 0, 100);
  applyNumberProperty("[data-y]", "y", "bottom", "%", 0, 100);
}

function applyNumberProperty(selector, datasetKey, property, suffix = "", min = 0, max = null) {
  document.querySelectorAll(selector).forEach((element) => {
    const raw = element.dataset[datasetKey];
    const parsed = Number.parseFloat(raw);
    if (!Number.isFinite(parsed)) return;
    const clamped = max === null ? Math.max(min, parsed) : Math.min(max, Math.max(min, parsed));
    applyDynamicClass(element, property, `${clamped}${suffix}`);
  });
}

function applyColorProperty(selector, datasetKey, property) {
  document.querySelectorAll(selector).forEach((element) => {
    const value = (element.dataset[datasetKey] || "").trim();
    if (!COLOR_RE.test(value)) return;
    applyDynamicClass(element, property, value);
  });
}

function applyWinsPieGradients() {
  document.querySelectorAll("[data-wins-pie]").forEach((pie) => {
    const slices = Array.from(pie.querySelectorAll("[data-wins-bg][data-wins-count]"))
      .map((slice) => {
        const color = (slice.dataset.winsBg || "").trim();
        const count = Number.parseFloat(slice.dataset.winsCount);
        if (!COLOR_RE.test(color) || !Number.isFinite(count) || count <= 0) return null;
        return { color, count };
      })
      .filter(Boolean);

    const total = slices.reduce((sum, slice) => sum + slice.count, 0);
    if (total <= 0) return;

    let cursor = 0;
    const stops = slices.map((slice, index) => {
      const start = cursor;
      cursor += (slice.count / total) * 100;
      const end = index + 1 === slices.length ? 100 : cursor;
      return `${slice.color} ${start.toFixed(4)}% ${end.toFixed(4)}%`;
    });

    applyDynamicClass(pie, "background", `conic-gradient(${stops.join(", ")})`);
  });
}

function applyDynamicClass(element, property, value) {
  const className = `is-dynamic-style-${hash(`${property}:${value}`)}`;
  ensureRule(className, property, value);
  element.classList.add(className);
}

function ensureRule(className, property, value) {
  if (rules.has(className)) return;
  rules.add(className);
  const sheet = ensureStyleSheet();
  sheet.append(`.${className} { ${property}: ${value}; }\n`);
}

function ensureStyleSheet() {
  if (styleSheet) return styleSheet;
  styleSheet = document.createElement("style");
  styleSheet.setAttribute("data-krab-dynamic-styles", "");
  document.head.append(styleSheet);
  return styleSheet;
}

function hash(value) {
  let out = 5381;
  for (let i = 0; i < value.length; i += 1) {
    out = (out * 33) ^ value.charCodeAt(i);
  }
  return (out >>> 0).toString(36);
}

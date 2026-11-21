import { analyticsDebugEnabled } from "./session.js";

const DEDUPE_WINDOW_MS = 1500;
const MAX_QUEUE_LENGTH = 50;
const MAX_STRING_LENGTH = 120;
const UUID_RE = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;
const SAFE_TOKEN_RE = /^[a-z0-9][a-z0-9_-]{0,119}$/i;

const queue = [];
const recentEvents = {};
let flushTimer = null;
let initialized = false;

function enumParam(allowedValues) {
  const allowed = Object.fromEntries(allowedValues.map((value) => [value, true]));
  return (value) => {
    value = normalizeToken(value);
    return allowed[value] ? value : null;
  };
}

function idParam(value) {
  value = normalizeString(value);
  if (!value) return null;
  if (UUID_RE.test(value)) return value.toLowerCase();
  if (SAFE_TOKEN_RE.test(value)) return value;
  return null;
}

const EVENT_CONTRACTS = {
  login_start: { params: { method: enumParam(["email", "github", "google", "oauth"]) } },
  login_success: { params: { method: enumParam(["email", "github", "google", "oauth"]) } },
  battle_open: { params: { battle_id: idParam } },
  claim_open: { params: { claim_id: idParam } },
  cli_install_click: {
    params: { source: enumParam(["home", "about", "battle", "docs", "unknown"]) }
  }
};

const RESERVED_OR_SENSITIVE_KEYS = {
  email: true,
  token: true,
  voter_token: true,
  jwt: true,
  name: true,
  display_name: true,
  username: true,
  user_name: true,
  password: true,
  secret: true,
  code: true,
  claim: true,
  title: true,
  description: true,
  body: true,
  text: true
};

export function normalizeString(value) {
  if (value === undefined || value === null) return null;
  if (typeof value !== "string" && typeof value !== "number" && typeof value !== "boolean") {
    return null;
  }
  value = stripControlCharacters(String(value)).replace(/\s+/g, " ").trim();
  if (!value) return null;
  return value.slice(0, MAX_STRING_LENGTH);
}

function stripControlCharacters(value) {
  return Array.from(value, (char) => {
    const code = char.charCodeAt(0);
    return code <= 31 || code === 127 ? " " : char;
  }).join("");
}

function normalizeToken(value) {
  value = normalizeString(value);
  return value ? value.toLowerCase() : null;
}

function isSensitiveKey(key) {
  key = String(key || "").toLowerCase();
  return RESERVED_OR_SENSITIVE_KEYS[key] === true;
}

function currentPath() {
  return safePath(window.location.pathname);
}

export function pageLocation() {
  return window.location.origin + window.location.pathname + window.location.search;
}

export function pagePath() {
  return currentPath();
}

export function pageTitle() {
  const meta = document.querySelector('meta[name="krab-analytics-title"]');
  let title = meta && meta.getAttribute("content");
  title = normalizeString(title);
  if (title) return title;
  title = normalizeString(document.title);
  return title || "KrabArena";
}

function safePath(path) {
  path = normalizeString(path) || "/";
  path = path.split("?")[0].split("#")[0];
  if (path === "/claims/standalone") return "/claims/standalone";
  if (path.startsWith("/battles/")) return "/battles/:battle";
  if (path.startsWith("/claims/")) return "/claims/:claim";
  if (path.startsWith("/agents/")) return "/agents/:agent";
  if (path.startsWith("/docs/")) return "/docs/:doc";
  if (path.startsWith("/login/callback")) return "/login/callback";
  return path;
}

function commonParams() {
  return {
    page_location: pageLocation(),
    page_title: pageTitle(),
    page_path: currentPath()
  };
}

function normalizeParams(eventName, params) {
  const contract = EVENT_CONTRACTS[eventName];
  const normalized = commonParams();
  const schema = contract.params || {};

  params = params || {};
  Object.keys(schema).forEach((key) => {
    if (isSensitiveKey(key)) return;
    const value = schema[key](params[key]);
    if (value !== null) normalized[key] = value;
  });

  return normalized;
}

function canSend() {
  return typeof window.gtag === "function";
}

function debugLog(message, payload) {
  if (!analyticsDebugEnabled() || !window.console || typeof window.console.info !== "function") {
    return;
  }
  window.console.info(`[krabAnalytics] ${message}`, payload || "");
}

function eventKey(eventName, payload) {
  return `${eventName}:${JSON.stringify(payload)}`;
}

function isDuplicate(key) {
  const now = Date.now();
  const lastSeenAt = recentEvents[key] || 0;
  recentEvents[key] = now;

  Object.keys(recentEvents).forEach((existingKey) => {
    if (now - recentEvents[existingKey] > DEDUPE_WINDOW_MS) {
      delete recentEvents[existingKey];
    }
  });

  return now - lastSeenAt < DEDUPE_WINDOW_MS;
}

function scheduleFlush() {
  if (flushTimer !== null) return;
  flushTimer = window.setTimeout(flushQueue, 0);
}

function flushQueue() {
  if (flushTimer !== null) {
    window.clearTimeout(flushTimer);
    flushTimer = null;
  }
  if (!queue.length) return;
  if (!canSend()) {
    debugLog("gtag unavailable, dropping queued events", { count: queue.length });
    queue.length = 0;
    return;
  }

  const pending = queue.splice(0, queue.length);
  pending.forEach((item) => {
    try {
      window.gtag("event", item.eventName, item.payload);
      debugLog(`event sent: ${item.eventName}`, item.payload);
    } catch (error) {
      debugLog(`event failed: ${item.eventName}`, error);
    }
  });
}

export function track(eventName, params) {
  try {
    eventName = normalizeToken(eventName);
    if (!eventName || !EVENT_CONTRACTS[eventName]) {
      debugLog("blocked unknown event", eventName);
      return false;
    }

    const payload = normalizeParams(eventName, params);
    const key = eventKey(eventName, payload);
    if (isDuplicate(key)) {
      debugLog(`blocked duplicate event: ${eventName}`, payload);
      return false;
    }

    if (queue.length >= MAX_QUEUE_LENGTH) queue.shift();
    queue.push({ eventName, payload });
    scheduleFlush();
    return true;
  } catch (error) {
    debugLog("event enqueue failed", error);
    return false;
  }
}

export function isEnabled() {
  return canSend();
}

function gaIdFromMeta() {
  const meta = document.querySelector('meta[name="krab-ga-id"]');
  const value = meta && meta.getAttribute("content");
  return /^[A-Z0-9-]+$/.test(value || "") ? value : null;
}

function loadGtag(gaId) {
  window.dataLayer = window.dataLayer || [];
  window.gtag =
    window.gtag ||
    function gtag() {
      window.dataLayer.push(arguments);
    };
  window.gtag("js", new Date());
  window.gtag("config", gaId, {
    page_location: pageLocation(),
    page_path: pagePath(),
    page_title: pageTitle()
  });

  if (document.querySelector(`script[data-krab-ga="${gaId}"]`)) return;
  const script = document.createElement("script");
  script.async = true;
  script.src = `https://www.googletagmanager.com/gtag/js?id=${encodeURIComponent(gaId)}`;
  script.dataset.krabGa = gaId;
  document.head.appendChild(script);
}

export function initAnalytics() {
  if (initialized) return;
  initialized = true;
  const api = Object.freeze({
    isEnabled,
    pageLocation,
    pagePath,
    pageTitle,
    track
  });
  window.krabAnalytics = api;
  const gaId = gaIdFromMeta();
  if (gaId) loadGtag(gaId);
}

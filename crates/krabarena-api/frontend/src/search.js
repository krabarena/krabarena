// Search UX for the single header search bar plus the /search results
// page: the AI toggle, typeahead suggestions, the themed kind dropdown,
// the clear button, "show more" in-place loading, and match
// highlighting.
//
// There is one canonical search bar (in the header, on every page). The
// AI toggle is a localStorage preference reflected in a hidden `ai`
// field; it does NOT re-run the search on click — the new mode applies
// on the next Search submit. On /search the bar is pre-filled from the
// URL so it reflects the active query.

import { searchSuggest } from "./api.js";
import { createDataTable } from "./data-table.js";

const AI_PREF_KEY = "krab_search_ai";
const DEBOUNCE_MS = 220;
const MIN_QUERY_LEN = 2;
const MIN_TERM_LEN = 2;
const RESULTS_PAGE_SIZE = 20;
const MORE_CEILING = 100;

export function initSearch() {
  const boxes = Array.from(document.querySelectorAll("[data-search-box]"));
  if (boxes.length) {
    // The /search page reflects an explicit `?ai=` in its results, so
    // let that drive the stored preference when present; otherwise fall
    // back to the stored value (default on).
    try {
      const params = new URLSearchParams(window.location.search);
      if (params.has("ai")) setAiPref(parseFlag(params.get("ai")));
    } catch {
      // Malformed URL — fall through to the stored preference.
    }
    const aiOn = getAiPref();
    boxes.forEach((box) => setupBox(box, aiOn));
    prefillFromUrl(boxes);
  }
  initKindDropdowns();
  initShowMore();
  highlightResults(document, currentTerms());
}

function getAiPref() {
  try {
    const raw = window.localStorage.getItem(AI_PREF_KEY);
    return raw === null ? true : raw === "1";
  } catch {
    return true;
  }
}

function setAiPref(on) {
  try {
    window.localStorage.setItem(AI_PREF_KEY, on ? "1" : "0");
  } catch {
    // Storage unavailable — in-DOM state still works for this page.
  }
}

function parseFlag(value) {
  const v = (value || "").trim().toLowerCase();
  return !(v === "0" || v === "false" || v === "off" || v === "no");
}

function setupBox(box, aiOn) {
  const input = box.querySelector("[data-search-input]");
  const toggle = box.querySelector("[data-search-ai-toggle]");
  const aiField = box.querySelector("[data-search-ai-field]");
  const suggestEl = box.querySelector("[data-search-suggest]");
  const kindField = box.querySelector("[name='kind']");
  const clearBtn = box.querySelector("[data-search-clear]");
  if (!input) return;

  applyAiState(toggle, aiField, aiOn);

  if (toggle) {
    toggle.addEventListener("click", () => {
      const next = !(aiField ? aiField.value === "1" : toggle.classList.contains("is-on"));
      setAiPref(next);
      // Reflect across every box; do NOT submit — applies on next Search.
      document.querySelectorAll("[data-search-box]").forEach((other) => {
        applyAiState(
          other.querySelector("[data-search-ai-toggle]"),
          other.querySelector("[data-search-ai-field]"),
          next
        );
      });
      if (suggestEl && input.value.trim().length >= MIN_QUERY_LEN) {
        runSuggest(input, suggestEl, kindField, aiField);
      }
    });
  }

  if (clearBtn) {
    const syncClear = () => {
      clearBtn.hidden = input.value.length === 0;
    };
    input.addEventListener("input", syncClear);
    clearBtn.addEventListener("click", () => {
      input.value = "";
      syncClear();
      if (suggestEl) {
        suggestEl.hidden = true;
        suggestEl.replaceChildren();
      }
      input.focus();
    });
    syncClear();
  }

  if (suggestEl) {
    wireSuggestions(box, input, suggestEl, kindField, aiField);
  }
}

function applyAiState(toggle, aiField, on) {
  if (aiField) aiField.value = on ? "1" : "0";
  if (!toggle) return;
  toggle.classList.toggle("is-on", on);
  toggle.classList.toggle("is-off", !on);
  toggle.setAttribute("aria-pressed", on ? "true" : "false");
}

// Pre-fill the bar from the URL so the single header bar reflects the
// active query on /search (q + kind; ai is already synced above).
function prefillFromUrl(boxes) {
  let params;
  try {
    params = new URLSearchParams(window.location.search);
  } catch {
    return;
  }
  const q = params.get("q");
  if (q === null) return;
  boxes.forEach((box) => {
    const input = box.querySelector("[data-search-input]");
    if (input && !input.value) {
      input.value = q;
      const clearBtn = box.querySelector("[data-search-clear]");
      if (clearBtn) clearBtn.hidden = input.value.length === 0;
    }
  });
  const kind = params.get("kind");
  if (kind) setKindValue(kind);
}

// ---------------------------------------------------------------------------
// Suggestions
// ---------------------------------------------------------------------------

function wireSuggestions(box, input, suggestEl, kindField, aiField) {
  let timer = null;
  let activeIndex = -1;

  const close = () => {
    suggestEl.hidden = true;
    suggestEl.replaceChildren();
    activeIndex = -1;
  };

  const items = () => Array.from(suggestEl.querySelectorAll(".c-suggest-item"));

  const setActive = (index) => {
    const all = items();
    if (!all.length) return;
    activeIndex = (index + all.length) % all.length;
    all.forEach((el, i) => el.classList.toggle("is-active", i === activeIndex));
  };

  input.addEventListener("input", () => {
    if (timer) clearTimeout(timer);
    const q = input.value.trim();
    if (q.length < MIN_QUERY_LEN) {
      close();
      return;
    }
    timer = setTimeout(() => {
      runSuggest(input, suggestEl, kindField, aiField).then(() => {
        activeIndex = -1;
      });
    }, DEBOUNCE_MS);
  });

  input.addEventListener("keydown", (event) => {
    const all = items();
    if (event.key === "ArrowDown") {
      if (!all.length) return;
      event.preventDefault();
      setActive(activeIndex + 1);
    } else if (event.key === "ArrowUp") {
      if (!all.length) return;
      event.preventDefault();
      setActive(activeIndex - 1);
    } else if (event.key === "Enter") {
      if (activeIndex >= 0 && all[activeIndex]) {
        event.preventDefault();
        window.location.href = all[activeIndex].getAttribute("href");
      }
    } else if (event.key === "Escape") {
      close();
    }
  });

  document.addEventListener("click", (event) => {
    if (!box.contains(event.target)) close();
  });
}

let suggestSeq = 0;

async function runSuggest(input, suggestEl, kindField, aiField) {
  const q = input.value.trim();
  if (q.length < MIN_QUERY_LEN) {
    suggestEl.hidden = true;
    return;
  }
  const ai = aiField ? aiField.value === "1" : true;
  const kind = kindField ? kindField.value : "all";
  const seq = ++suggestSeq;
  try {
    const res = await searchSuggest({ q, kind, ai });
    if (seq !== suggestSeq) return;
    const data = res?.data?.data || [];
    renderSuggestions(suggestEl, data, splitTerms(q));
  } catch {
    if (seq === suggestSeq) suggestEl.hidden = true;
  }
}

function renderSuggestions(suggestEl, data, terms) {
  if (!data.length) {
    const empty = document.createElement("div");
    empty.className = "c-suggest-empty";
    empty.textContent = "No suggestions";
    suggestEl.replaceChildren(empty);
    suggestEl.hidden = false;
    return;
  }
  const frag = document.createDocumentFragment();
  data.forEach((hit) => {
    const a = document.createElement("a");
    a.className = "c-suggest-item";
    a.setAttribute("role", "option");
    a.href = hit.url;

    const kind = document.createElement("span");
    kind.className = "c-suggest-kind";
    kind.textContent = hit.kind;
    a.append(kind);

    const title = document.createElement("span");
    title.className = "c-suggest-title";
    title.textContent = hit.title;
    highlightEl(title, terms);
    a.append(title);

    frag.append(a);
  });
  suggestEl.replaceChildren(frag);
  suggestEl.hidden = false;
}

// ---------------------------------------------------------------------------
// Themed kind dropdown
// ---------------------------------------------------------------------------

function initKindDropdowns() {
  document.querySelectorAll("[data-kind-dd]").forEach((dd) => {
    const trigger = dd.querySelector("[data-kind-trigger]");
    const menu = dd.querySelector(".c-kind-dd-menu");
    const valueInput = dd.querySelector("[data-kind-value]");
    const label = dd.querySelector("[data-kind-label]");
    const options = Array.from(dd.querySelectorAll(".c-kind-dd-option"));
    if (!trigger || !menu || !valueInput || !label || !options.length) return;

    let activeIndex = -1;
    const setActive = (index) => {
      activeIndex = (index + options.length) % options.length;
      options.forEach((o, i) => o.classList.toggle("is-active", i === activeIndex));
    };
    const open = () => {
      menu.hidden = false;
      trigger.setAttribute("aria-expanded", "true");
      const sel = options.findIndex((o) => o.classList.contains("is-selected"));
      setActive(sel < 0 ? 0 : sel);
    };
    const close = () => {
      menu.hidden = true;
      trigger.setAttribute("aria-expanded", "false");
    };
    const isOpen = () => !menu.hidden;

    const select = (opt) => {
      valueInput.value = opt.getAttribute("data-kind-option");
      label.textContent = opt.textContent;
      options.forEach((o) => {
        const on = o === opt;
        o.classList.toggle("is-selected", on);
        o.setAttribute("aria-selected", on ? "true" : "false");
      });
      close();
      trigger.focus();
    };

    trigger.addEventListener("click", () => {
      if (isOpen()) close();
      else open();
    });

    trigger.addEventListener("keydown", (event) => {
      if (event.key === "Escape") {
        close();
        return;
      }
      if (!isOpen()) {
        if (event.key === "ArrowDown" || event.key === "Enter" || event.key === " ") {
          event.preventDefault();
          open();
        }
        return;
      }
      if (event.key === "ArrowDown") {
        event.preventDefault();
        setActive(activeIndex + 1);
      } else if (event.key === "ArrowUp") {
        event.preventDefault();
        setActive(activeIndex - 1);
      } else if (event.key === "Enter" || event.key === " ") {
        event.preventDefault();
        if (options[activeIndex]) select(options[activeIndex]);
      }
    });

    options.forEach((opt, i) => {
      opt.addEventListener("mouseenter", () => setActive(i));
      opt.addEventListener("click", () => select(opt));
    });

    document.addEventListener("click", (event) => {
      if (!dd.contains(event.target)) close();
    });
  });
}

// Set the kind dropdown(s) to a value programmatically (URL prefill).
function setKindValue(value) {
  document.querySelectorAll("[data-kind-dd]").forEach((dd) => {
    const valueInput = dd.querySelector("[data-kind-value]");
    const label = dd.querySelector("[data-kind-label]");
    const options = Array.from(dd.querySelectorAll(".c-kind-dd-option"));
    const match = options.find((o) => o.getAttribute("data-kind-option") === value);
    if (!valueInput || !label || !match) return;
    valueInput.value = value;
    label.textContent = match.textContent;
    options.forEach((o) => {
      const on = o === match;
      o.classList.toggle("is-selected", on);
      o.setAttribute("aria-selected", on ? "true" : "false");
    });
  });
}

// ---------------------------------------------------------------------------
// "Show more" — switch the result list to a windowed Data Table.
//
// Clicking "Show more" fetches the full ranked set once (capped at
// MORE_CEILING) and hands it to `createDataTable`, which renders ONE page
// at a time via `replaceChildren`. The DOM stays a constant size instead
// of growing a card per result as the old append-forever loop did. With
// JS off, the button is a plain `<a href>` to the next server-rendered
// page, so the no-JS path is unaffected.
// ---------------------------------------------------------------------------

function initShowMore() {
  document.querySelectorAll("[data-search-more]").forEach((btn) => {
    btn.addEventListener("click", (event) => {
      event.preventDefault();
      switchToWindowed(btn);
    });
  });
}

async function switchToWindowed(btn) {
  if (btn.dataset.loading === "1") return;
  const list = document.querySelector("[data-search-results]");
  const moreWrap = btn.closest(".c-search-more");
  if (!list || !moreWrap) return;

  const q = btn.getAttribute("data-more-q") || "";
  const kind = btn.getAttribute("data-more-kind") || "all";
  const ai = btn.getAttribute("data-more-ai") || "0";
  if (!q) return;

  btn.dataset.loading = "1";
  const original = btn.textContent;
  btn.textContent = "Loading…";

  let data;
  try {
    const params = new URLSearchParams({ q, kind, ai, limit: String(MORE_CEILING) });
    const res = await fetch(`/v1/search?${params.toString()}`, {
      headers: { Accept: "application/json" }
    });
    const json = await res.json();
    data = (json && json.data) || [];
  } catch {
    btn.textContent = original;
    btn.dataset.loading = "0";
    return;
  }

  const terms = splitTerms(q);
  const pager = buildResultsPager();
  moreWrap.replaceWith(pager.root);
  createDataTable({
    body: list,
    items: data,
    pageSize: RESULTS_PAGE_SIZE,
    renderRow: (hit) => renderResultItem(hit, terms),
    prevBtn: pager.prev,
    nextBtn: pager.next,
    status: pager.status,
    pager: pager.root
  });
}

// Build the Prev / status / Next controls that replace the "Show more"
// button once the list becomes a windowed table.
function buildResultsPager() {
  const root = document.createElement("div");
  root.className = "pagination c-search-more";

  const prev = document.createElement("button");
  prev.type = "button";
  prev.className = "btn";
  prev.textContent = "← Prev";

  const status = document.createElement("span");
  status.className = "page-indicator";

  const next = document.createElement("button");
  next.type = "button";
  next.className = "btn";
  next.textContent = "Next →";

  root.append(prev, status, next);
  return { root, prev, next, status };
}

// Render one result as a site-style `.post` card (matches the
// server-rendered markup in search.html).
function renderResultItem(hit, terms) {
  const article = document.createElement("article");
  article.className = "post c-search-result";
  if (hit.semantic) article.setAttribute("data-semantic", "1");

  const body = document.createElement("div");
  body.className = "post-body";

  const meta = document.createElement("header");
  meta.className = "post-meta";
  const badge = document.createElement("span");
  badge.className = "badge c-kind-badge";
  badge.textContent = hit.kind;
  meta.append(badge);
  if (hit.semantic) {
    const tag = document.createElement("span");
    tag.className = "c-semantic-tag";
    tag.title = "Matched by meaning (AI), not literal text";
    tag.textContent = "semantic";
    meta.append(tag);
  }
  body.append(meta);

  const h2 = document.createElement("h2");
  h2.className = "post-title";
  const link = document.createElement("a");
  link.className = "c-link-plain c-search-title";
  link.href = hit.url;
  link.textContent = hit.title;
  highlightEl(link, terms);
  h2.append(link);
  body.append(h2);

  if (hit.snippet) {
    const snippet = document.createElement("p");
    snippet.className = "post-summary c-search-snippet";
    snippet.textContent = hit.snippet;
    highlightEl(snippet, terms);
    body.append(snippet);
  }

  article.append(body);
  return article;
}

// ---------------------------------------------------------------------------
// Highlighting
// ---------------------------------------------------------------------------

function currentTerms() {
  try {
    const q = new URLSearchParams(window.location.search).get("q");
    if (q) return splitTerms(q);
  } catch {
    // ignore
  }
  return [];
}

function splitTerms(q) {
  return (q || "")
    .trim()
    .split(/\s+/)
    .filter((t) => t.length >= MIN_TERM_LEN);
}

function escapeRegex(s) {
  return s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

// Wrap literal term matches in <mark>, building DOM nodes (never
// innerHTML) so it's XSS-safe regardless of the source text.
function highlightEl(el, terms) {
  if (!el || !terms.length) return;
  const text = el.textContent;
  const re = new RegExp(`(${terms.map(escapeRegex).join("|")})`, "gi");
  if (!re.test(text)) return;
  re.lastIndex = 0;
  const frag = document.createDocumentFragment();
  let last = 0;
  let m;
  while ((m = re.exec(text)) !== null) {
    if (m.index > last) frag.append(document.createTextNode(text.slice(last, m.index)));
    const mark = document.createElement("mark");
    mark.textContent = m[0];
    frag.append(mark);
    last = m.index + m[0].length;
    if (m.index === re.lastIndex) re.lastIndex += 1;
  }
  if (last < text.length) frag.append(document.createTextNode(text.slice(last)));
  el.replaceChildren(frag);
}

function highlightResults(root, terms) {
  if (!terms.length) return;
  root
    .querySelectorAll(".c-search-result .c-search-title, .c-search-result .c-search-snippet")
    .forEach((el) => highlightEl(el, terms));
}

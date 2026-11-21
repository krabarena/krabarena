/**
 * sortable-tables.js — turn every server-rendered `.markdown-body table`
 * into a click-to-sort table, with auto-detection of numeric columns.
 *
 * KrabReviewer editorials carry markdown tables (Leaderboard, Top Evals,
 * etc.) that pulldown-cmark renders as plain `<table>`. Readers want to
 * re-sort by Wins, by Score, by any eval column. This script tags every
 * such table at load time and adds click handlers on `<th>` elements.
 *
 * Numeric detection runs once per column on first sort: tries to parse
 * the cell text after stripping `, %, $, →, parens, dashes`. If ≥ half
 * of non-empty cells parse as numbers, the column sorts numerically;
 * otherwise lexicographically. `—` (em-dash, KrabReviewer's "no data"
 * sentinel) sorts last in both directions.
 *
 * Self-contained, no deps. ~80 LOC.
 */
(function () {
  "use strict";

  const NUMERIC_THRESHOLD = 0.5; // ≥ 50% of non-empty cells must parse

  function parseNumber(text) {
    if (text == null) return NaN;
    let s = String(text).trim();
    if (s === "" || s === "—" || s === "-" || s === "n/a") return NaN;
    // Strip thousand separators, currency, percent, parens.
    // Keep only the first numeric run so cells like "82.0 (100%)"
    // sort by the raw value (82.0), not the normalized %.
    const m = s.replace(/,/g, "").match(/-?\d+(?:\.\d+)?/);
    return m ? parseFloat(m[0]) : NaN;
  }

  function detectNumeric(cells) {
    if (cells.length === 0) return false;
    let parseable = 0;
    let nonEmpty = 0;
    for (const c of cells) {
      const t = (c.textContent || "").trim();
      if (t === "" || t === "—") continue;
      nonEmpty++;
      if (!isNaN(parseNumber(t))) parseable++;
    }
    return nonEmpty > 0 && parseable / nonEmpty >= NUMERIC_THRESHOLD;
  }

  function sortRows(rows, colIdx, dir, numeric) {
    const factor = dir === "asc" ? 1 : -1;
    return rows.slice().sort((a, b) => {
      const at = (a.children[colIdx]?.textContent || "").trim();
      const bt = (b.children[colIdx]?.textContent || "").trim();
      // Sentinels (em-dash / empty) always sort last regardless of dir.
      const aEmpty = at === "" || at === "—";
      const bEmpty = bt === "" || bt === "—";
      if (aEmpty && bEmpty) return 0;
      if (aEmpty) return 1;
      if (bEmpty) return -1;
      if (numeric) {
        const an = parseNumber(at);
        const bn = parseNumber(bt);
        if (isNaN(an) && isNaN(bn)) return 0;
        if (isNaN(an)) return 1;
        if (isNaN(bn)) return -1;
        return factor * (an - bn);
      }
      return factor * at.localeCompare(bt, undefined, { numeric: true, sensitivity: "base" });
    });
  }

  function makeSortable(table) {
    const ths = table.querySelectorAll("thead th");
    const tbody = table.querySelector("tbody");
    if (!ths.length || !tbody) return;
    table.classList.add("sortable");
    ths.forEach((th, idx) => {
      th.setAttribute("data-sortable", "true");
      th.addEventListener("click", () => {
        const rows = Array.from(tbody.querySelectorAll("tr"));
        if (rows.length < 2) return;
        const colCells = rows.map((r) => r.children[idx]).filter(Boolean);
        const numeric = detectNumeric(colCells);
        const cur = th.getAttribute("data-sort-dir");
        const next = cur === "asc" ? "desc" : "asc";
        // Clear sort indicators on siblings.
        ths.forEach((other) => other.removeAttribute("data-sort-dir"));
        th.setAttribute("data-sort-dir", next);
        const sorted = sortRows(rows, idx, next, numeric);
        for (const r of sorted) tbody.appendChild(r);
      });
    });
  }

  /**
   * Click-to-expand for the last column (Notes) of every editorial
   * table. CSS clamps the cell to one line by default; this toggles
   * an `.expanded` class on click so a long note can be read in
   * full without permanently blowing up row heights. Listener is
   * delegated on the table so dynamically-added rows still work.
   */
  function makeNotesExpandable(table) {
    table.addEventListener("click", (ev) => {
      const td = ev.target.closest("td");
      if (!td || td.parentElement.parentElement.tagName !== "TBODY") return;
      const tr = td.parentElement;
      // Only the last cell in the row is the Notes column.
      if (td !== tr.children[tr.children.length - 1]) return;
      td.classList.toggle("expanded");
    });
  }

  function init() {
    document.querySelectorAll(".markdown-body table").forEach((t) => {
      makeSortable(t);
      makeNotesExpandable(t);
    });
  }

  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", init);
  } else {
    init();
  }
})();

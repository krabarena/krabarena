/**
 * sort.js — vanilla click-to-sort for the aggregated battle
 * leaderboard (`.lb.agg-lb`).
 *
 * Distinct from `sortable-tables.js` (which sorts every markdown-body
 * table by re-parsing cell text). This script reads server-rendered
 * `data-sort-value="<float>"` attributes so it never has to parse
 * formatted text like `"$0.0227"` — the Rust side decided the sort
 * key when it built the row. Three-state cycle per column:
 *
 *   click 1 → asc
 *   click 2 → desc
 *   click 3 → default (wins desc / avg-rank asc — the server's initial order)
 *
 * Sort indicators (▲ / ▼) appear in the active column header via
 * the inline `.sort-glyph` span that the template ships next to each
 * sortable column. Text columns (no `.sortable` class on the `<th>`)
 * never get a click handler.
 *
 * No deps. ~140 LOC.
 */
(function () {
  "use strict";

  /** Read the data-sort-value attr from a cell as a float, or NaN. */
  function cellSortValue(td) {
    if (!td) return NaN;
    const raw = td.getAttribute("data-sort-value");
    if (raw == null || raw === "") return NaN;
    const n = parseFloat(raw);
    return isFinite(n) ? n : NaN;
  }

  /** Compare two table rows numerically by the given column. */
  function compareRowsNumeric(a, b, colIdx, dir) {
    const av = cellSortValue(a.children[colIdx]);
    const bv = cellSortValue(b.children[colIdx]);
    const aMissing = isNaN(av);
    const bMissing = isNaN(bv);
    // Missing values always sort last regardless of direction.
    if (aMissing && bMissing) return 0;
    if (aMissing) return 1;
    if (bMissing) return -1;
    return dir === "asc" ? av - bv : bv - av;
  }

  /** Restore the original row order from the snapshot we captured on init. */
  function restoreDefault(tbody, defaultOrder) {
    for (const r of defaultOrder) tbody.appendChild(r);
  }

  /** Update the ▲ / ▼ glyphs on each sortable header. */
  function updateGlyphs(ths, activeIdx, dir) {
    ths.forEach((th, idx) => {
      const g = th.querySelector(".sort-glyph");
      if (!g) return;
      if (idx === activeIdx && dir) {
        g.textContent = dir === "asc" ? "▲" : "▼";
      } else {
        g.textContent = "";
      }
    });
  }

  function wire(table) {
    const tbody = table.querySelector("tbody");
    if (!tbody) return;
    const ths = Array.from(table.querySelectorAll("thead th"));
    if (!ths.length) return;
    // Snapshot the default order at load time so the third click can
    // restore it without re-fetching the page.
    const defaultOrder = Array.from(tbody.querySelectorAll("tr"));

    // Per-table state: which column is active + which direction.
    // Stored on the table dataset so the handler closures stay
    // stateless.
    table.dataset.sortCol = "";
    table.dataset.sortDir = "";

    ths.forEach((th, idx) => {
      if (!th.classList.contains("sortable")) return;
      th.style.cursor = "pointer";
      th.setAttribute("role", "button");
      th.setAttribute("tabindex", "0");
      const handler = () => {
        const curCol = table.dataset.sortCol;
        const curDir = table.dataset.sortDir;
        let nextDir;
        if (curCol !== String(idx) || curDir === "") {
          nextDir = "asc";
        } else if (curDir === "asc") {
          nextDir = "desc";
        } else {
          // Third click on the same column → back to default.
          nextDir = "";
        }
        if (nextDir === "") {
          table.dataset.sortCol = "";
          table.dataset.sortDir = "";
          restoreDefault(tbody, defaultOrder);
          updateGlyphs(ths, -1, "");
          return;
        }
        const rows = Array.from(tbody.querySelectorAll("tr"));
        rows.sort((a, b) => compareRowsNumeric(a, b, idx, nextDir));
        for (const r of rows) tbody.appendChild(r);
        table.dataset.sortCol = String(idx);
        table.dataset.sortDir = nextDir;
        updateGlyphs(ths, idx, nextDir);
      };
      th.addEventListener("click", handler);
      th.addEventListener("keydown", (ev) => {
        if (ev.key === "Enter" || ev.key === " ") {
          ev.preventDefault();
          handler();
        }
      });
    });
  }

  function init() {
    document.querySelectorAll("table.agg-lb").forEach(wire);
  }

  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", init);
  } else {
    init();
  }
})();

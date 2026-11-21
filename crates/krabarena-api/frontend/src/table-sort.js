const NUMERIC_THRESHOLD = 0.5;

function parseNumber(text) {
  if (text == null) return NaN;
  const value = String(text).trim();
  if (value === "" || value === "-" || value === "n/a") return NaN;
  const match = value.replace(/,/g, "").match(/-?\d+(?:\.\d+)?/);
  return match ? parseFloat(match[0]) : NaN;
}

function detectNumeric(cells) {
  if (cells.length === 0) return false;
  let parseable = 0;
  let nonEmpty = 0;
  cells.forEach((cell) => {
    const text = (cell.textContent || "").trim();
    if (text === "" || text === "-") return;
    nonEmpty += 1;
    if (!Number.isNaN(parseNumber(text))) parseable += 1;
  });
  return nonEmpty > 0 && parseable / nonEmpty >= NUMERIC_THRESHOLD;
}

function sortRows(rows, colIdx, dir, numeric) {
  const factor = dir === "asc" ? 1 : -1;
  return rows.slice().sort((a, b) => {
    const at = (a.children[colIdx]?.textContent || "").trim();
    const bt = (b.children[colIdx]?.textContent || "").trim();
    const aEmpty = at === "" || at === "-";
    const bEmpty = bt === "" || bt === "-";
    if (aEmpty && bEmpty) return 0;
    if (aEmpty) return 1;
    if (bEmpty) return -1;
    if (numeric) {
      const an = parseNumber(at);
      const bn = parseNumber(bt);
      if (Number.isNaN(an) && Number.isNaN(bn)) return 0;
      if (Number.isNaN(an)) return 1;
      if (Number.isNaN(bn)) return -1;
      return factor * (an - bn);
    }
    return factor * at.localeCompare(bt, undefined, { numeric: true, sensitivity: "base" });
  });
}

function makeMarkdownTableSortable(table) {
  const headers = table.querySelectorAll("thead th");
  const tbody = table.querySelector("tbody");
  if (!headers.length || !tbody) return;
  table.classList.add("sortable");
  headers.forEach((header, idx) => {
    header.setAttribute("data-sortable", "true");
    header.addEventListener("click", () => {
      const rows = Array.from(tbody.querySelectorAll("tr"));
      if (rows.length < 2) return;
      const numeric = detectNumeric(rows.map((row) => row.children[idx]).filter(Boolean));
      const next = header.getAttribute("data-sort-dir") === "asc" ? "desc" : "asc";
      headers.forEach((other) => other.removeAttribute("data-sort-dir"));
      header.setAttribute("data-sort-dir", next);
      sortRows(rows, idx, next, numeric).forEach((row) => tbody.appendChild(row));
    });
  });
  table.addEventListener("click", (event) => {
    const td = event.target.closest("td");
    if (!td || td.parentElement.parentElement.tagName !== "TBODY") return;
    const tr = td.parentElement;
    if (td !== tr.children[tr.children.length - 1]) return;
    td.classList.toggle("expanded");
  });
}

function cellSortValue(td) {
  if (!td) return NaN;
  const raw = td.getAttribute("data-sort-value");
  if (raw == null || raw === "") return NaN;
  const value = parseFloat(raw);
  return Number.isFinite(value) ? value : NaN;
}

function compareRowsNumeric(a, b, colIdx, dir) {
  const av = cellSortValue(a.children[colIdx]);
  const bv = cellSortValue(b.children[colIdx]);
  const aMissing = Number.isNaN(av);
  const bMissing = Number.isNaN(bv);
  if (aMissing && bMissing) return 0;
  if (aMissing) return 1;
  if (bMissing) return -1;
  return dir === "asc" ? av - bv : bv - av;
}

function updateGlyphs(headers, activeIdx, dir) {
  headers.forEach((header, idx) => {
    const glyph = header.querySelector(".sort-glyph");
    if (!glyph) return;
    glyph.textContent = idx === activeIdx && dir ? (dir === "asc" ? "▲" : "▼") : "";
  });
}

function wireAggregatedLeaderboard(table) {
  const tbody = table.querySelector("tbody");
  const headers = Array.from(table.querySelectorAll("thead th"));
  if (!tbody || !headers.length) return;
  const defaultOrder = Array.from(tbody.querySelectorAll("tr"));
  table.dataset.sortCol = "";
  table.dataset.sortDir = "";

  headers.forEach((header, idx) => {
    if (!header.classList.contains("sortable")) return;
    header.setAttribute("role", "button");
    header.setAttribute("tabindex", "0");
    const handler = () => {
      const curCol = table.dataset.sortCol;
      const curDir = table.dataset.sortDir;
      let nextDir;
      if (curCol !== String(idx) || curDir === "") nextDir = "asc";
      else if (curDir === "asc") nextDir = "desc";
      else nextDir = "";
      if (nextDir === "") {
        table.dataset.sortCol = "";
        table.dataset.sortDir = "";
        defaultOrder.forEach((row) => tbody.appendChild(row));
        updateGlyphs(headers, -1, "");
        return;
      }
      const rows = Array.from(tbody.querySelectorAll("tr"));
      rows.sort((a, b) => compareRowsNumeric(a, b, idx, nextDir));
      rows.forEach((row) => tbody.appendChild(row));
      table.dataset.sortCol = String(idx);
      table.dataset.sortDir = nextDir;
      updateGlyphs(headers, idx, nextDir);
    };
    header.addEventListener("click", handler);
    header.addEventListener("keydown", (event) => {
      if (event.key === "Enter" || event.key === " ") {
        event.preventDefault();
        handler();
      }
    });
  });
}

export function initTableSort() {
  document.querySelectorAll(".markdown-body table").forEach(makeMarkdownTableSortable);
  document.querySelectorAll("table.agg-lb").forEach(wireAggregatedLeaderboard);
}

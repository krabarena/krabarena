// Windowed Data Table: render one page of an in-memory list at a time so
// the DOM stays a constant size no matter how many rows exist. Replaces
// the "append every row forever" pattern (profile agent list, search
// results) whose DOM grew without bound as more data loaded.
//
// The windowing maths (`tableWindow`) is a pure function so it can be
// unit-tested without a DOM; `createDataTable` is the thin DOM controller
// that wires Prev/Next buttons and re-renders the body via
// `replaceChildren` (the whole point: the old page's nodes are dropped,
// not kept).

const DEFAULT_PAGE_SIZE = 25;

/**
 * Compute the slice + navigation state for a page of `total` items.
 * `page` is clamped into range, so callers can pass an out-of-bounds
 * page (e.g. after the item count shrank) and get a valid window back.
 */
export function tableWindow(total, page, pageSize) {
  const size = Math.max(1, Math.floor(pageSize) || DEFAULT_PAGE_SIZE);
  const count = Math.max(0, Math.floor(total) || 0);
  const pageCount = Math.max(1, Math.ceil(count / size));
  const current = Math.min(Math.max(Math.floor(page) || 0, 0), pageCount - 1);
  const start = current * size;
  const end = Math.min(start + size, count);
  return {
    page: current,
    pageCount,
    start,
    end,
    hasPrev: current > 0,
    hasNext: current < pageCount - 1
  };
}

/**
 * Human label for the current window, e.g. "1–25 of 134". Empty string
 * for an empty list so the caller can hide the status line.
 */
export function tableStatusLabel(win, total) {
  if (!total) return "";
  return `${win.start + 1}–${win.end} of ${total}`;
}

/**
 * Mount a windowed table over an in-memory `items` array.
 *
 * @param {object} opts
 * @param {Element} opts.body       Container whose children ARE the current page.
 * @param {Array}   opts.items      Backing data (kept in memory; never all in DOM).
 * @param {(item, index) => Node} opts.renderRow  Builds one row element.
 * @param {number}  [opts.pageSize]
 * @param {Element} [opts.prevBtn]  Wired to go back a page; `disabled` reflects bounds.
 * @param {Element} [opts.nextBtn]  Wired to advance a page.
 * @param {Element} [opts.status]   Optional "1–25 of N" text target.
 * @param {Element} [opts.pager]    Optional wrapper hidden when everything fits one page.
 * @returns {{ render: () => void, setItems: (next: Array) => void, page: () => number }}
 */
export function createDataTable(opts) {
  const { body, renderRow, prevBtn, nextBtn, status, pager } = opts;
  const pageSize = opts.pageSize || DEFAULT_PAGE_SIZE;
  let items = opts.items || [];
  let page = 0;

  function render() {
    const win = tableWindow(items.length, page, pageSize);
    page = win.page;

    const frag = document.createDocumentFragment();
    for (let i = win.start; i < win.end; i += 1) {
      const row = renderRow(items[i], i);
      if (row) frag.append(row);
    }
    // The crux: swap the whole page out, never accumulate.
    body.replaceChildren(frag);

    if (status) status.textContent = tableStatusLabel(win, items.length);
    if (prevBtn) prevBtn.disabled = !win.hasPrev;
    if (nextBtn) nextBtn.disabled = !win.hasNext;
    if (pager) pager.hidden = win.pageCount <= 1;
  }

  function go(delta) {
    page += delta;
    render();
  }

  if (prevBtn) prevBtn.addEventListener("click", () => go(-1));
  if (nextBtn) nextBtn.addEventListener("click", () => go(1));

  render();

  return {
    render,
    page: () => page,
    setItems(next) {
      items = next || [];
      page = 0;
      render();
    }
  };
}

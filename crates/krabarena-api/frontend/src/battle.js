import { track } from "./analytics.js";

export function initBattlePage() {
  const page = document.querySelector("[data-battle-page]");
  if (!page) return;
  const battleId = page.getAttribute("data-battle-id");
  if (battleId) track("battle_open", { battle_id: battleId });
  initWinsGithubPanel();
}

function initWinsGithubPanel() {
  const panel = document.querySelector(".wins-panel");
  if (!panel) return;
  const body = panel.querySelector(".wins-github-body");
  if (!body) return;
  const segments = panel.querySelectorAll(".wins-pie-slice");
  const items = panel.querySelectorAll(".wins-legend-item");
  const pie = panel.querySelector(".wins-pie");
  const wrap = panel.querySelector(".wins-pie-wrap");
  const tooltip = panel.querySelector("[data-wins-tooltip]");

  // Slice angle ranges, mirroring dynamic-styles.js so the same conic-gradient
  // stops decide which solution the cursor lands on. Skip zero-win slices to
  // match the gradient (they contribute no arc).
  const ranges = [];
  {
    let cursor = 0;
    const live = Array.from(segments)
      .map((segment, index) => {
        const count = Number.parseFloat(segment.dataset.winsCount);
        return Number.isFinite(count) && count > 0 ? { index, count } : null;
      })
      .filter(Boolean);
    const total = live.reduce((sum, slice) => sum + slice.count, 0);
    if (total > 0) {
      live.forEach((slice, i) => {
        const start = cursor;
        cursor += (slice.count / total) * 100;
        const end = i === live.length - 1 ? 100 : cursor;
        const item = items[slice.index];
        const name = item?.querySelector(".wins-legend-name")?.textContent?.trim() || "";
        const pct = item?.querySelector(".wins-legend-pct")?.textContent?.trim() || "";
        ranges.push({ idx: slice.index, start, end, name, count: slice.count, pct });
      });
    }
  }

  function setFocus(idx) {
    const on = idx !== null;
    panel.classList.toggle("is-focus", on);
    segments.forEach((segment, index) => {
      segment.classList.toggle("is-dimmed", on && index !== idx);
      segment.classList.toggle("is-active", on && index === idx);
    });
    items.forEach((item, index) => {
      item.classList.toggle("is-active", on && index === idx);
    });
  }

  function rangeForIdx(idx) {
    return ranges.find((range) => range.idx === idx) || null;
  }

  function rangeForAngle(percent) {
    return (
      ranges.find((range) => percent >= range.start && percent < range.end) ||
      ranges[ranges.length - 1] ||
      null
    );
  }

  function showTooltip(range, clientX, clientY) {
    if (!tooltip || !wrap || !range) return;
    const count = Math.round(range.count);
    const noun = count === 1 ? "point" : "points";
    tooltip.textContent = `${range.name} · ${count} ${noun} · ${range.pct}`;
    tooltip.hidden = false;
    tooltip.setAttribute("aria-hidden", "false");
    const wrapRect = wrap.getBoundingClientRect();
    // Default: anchor above the pie, centered horizontally. If a cursor
    // position is supplied (pie hover), follow it; legend hover keeps the
    // anchored default so the tooltip doesn't jump off-screen.
    const tipRect = tooltip.getBoundingClientRect();
    const margin = 6;
    let left;
    let top;
    if (Number.isFinite(clientX) && Number.isFinite(clientY)) {
      left = clientX - wrapRect.left + 12;
      top = clientY - wrapRect.top - tipRect.height - margin;
    } else {
      left = wrapRect.width / 2 - tipRect.width / 2;
      top = -tipRect.height - margin;
    }
    // Keep inside the wrap horizontally so we don't clip against the panel edge.
    const maxLeft = wrapRect.width - tipRect.width - 2;
    if (left < 2) left = 2;
    if (left > maxLeft) left = Math.max(2, maxLeft);
    tooltip.style.left = `${left}px`;
    tooltip.style.top = `${top}px`;
    tooltip.classList.add("is-visible");
  }

  function hideTooltip() {
    if (!tooltip) return;
    tooltip.classList.remove("is-visible");
    tooltip.setAttribute("aria-hidden", "true");
    tooltip.hidden = true;
  }

  if (pie && ranges.length > 0) {
    pie.addEventListener("mousemove", (event) => {
      const rect = pie.getBoundingClientRect();
      const cx = rect.left + rect.width / 2;
      const cy = rect.top + rect.height / 2;
      const dx = event.clientX - cx;
      const dy = event.clientY - cy;
      const radius = Math.min(rect.width, rect.height) / 2;
      if (Math.hypot(dx, dy) > radius) {
        setFocus(null);
        hideTooltip();
        return;
      }
      // conic-gradient starts at 12 o'clock and runs clockwise; atan2(dx, -dy)
      // matches that origin so percentages line up with the gradient stops.
      let angle = Math.atan2(dx, -dy);
      if (angle < 0) angle += Math.PI * 2;
      const pct = (angle / (Math.PI * 2)) * 100;
      const range = rangeForAngle(pct);
      if (!range) return;
      setFocus(range.idx);
      showTooltip(range, event.clientX, event.clientY);
    });
    pie.addEventListener("mouseleave", () => {
      setFocus(null);
      hideTooltip();
    });
  }

  body.addEventListener("mouseover", (event) => {
    const target = event.target.closest("[data-wins-idx]");
    if (!target) return;
    const idx = Number(target.getAttribute("data-wins-idx"));
    setFocus(idx);
    if (target.classList.contains("wins-legend-item")) {
      showTooltip(rangeForIdx(idx));
    }
  });
  body.addEventListener("mouseout", (event) => {
    if (body.contains(event.relatedTarget)) return;
    setFocus(null);
    hideTooltip();
  });
}

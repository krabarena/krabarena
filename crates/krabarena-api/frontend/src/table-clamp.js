export function initTableClamp() {
  const selector = ".detail-card .prose tbody td, table.lb tbody td, table.arena-table tbody td";
  const clamps = [];
  document.querySelectorAll(selector).forEach((td) => {
    if (td.querySelector(":scope > .cell-clamp")) return;
    const div = document.createElement("div");
    div.className = "cell-clamp";
    while (td.firstChild) div.appendChild(td.firstChild);
    td.appendChild(div);
    clamps.push(div);
  });

  requestAnimationFrame(() => {
    clamps.forEach((div) => {
      if (div.scrollHeight > div.clientHeight + 1) div.dataset.clipped = "true";
    });
  });

  document.addEventListener("click", (event) => {
    const clamp = event.target.closest && event.target.closest("td > .cell-clamp");
    if (!clamp) return;
    if (event.target.closest("a, button, input, select, textarea, label")) return;
    clamp.dataset.expanded = clamp.dataset.expanded === "true" ? "false" : "true";
  });
}

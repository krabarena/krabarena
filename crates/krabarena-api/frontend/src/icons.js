export function iconSpriteUrl() {
  const meta = document.querySelector('meta[name="krab-icon-sprite"]');
  return (meta && meta.getAttribute("content")) || "/static/dist/icons/krab-icons.svg";
}

export function iconMarkup(name, label) {
  const aria = label ? `role="img" aria-label="${escapeAttr(label)}"` : 'aria-hidden="true"';
  return `<span class="icon icon-${escapeAttr(name)}" ${aria}></span>`;
}

function escapeAttr(value) {
  return String(value || "")
    .replace(/&/g, "&amp;")
    .replace(/"/g, "&quot;")
    .replace(/'/g, "&#39;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;");
}

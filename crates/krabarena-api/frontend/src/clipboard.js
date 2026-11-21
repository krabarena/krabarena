import { track } from "./analytics.js";

export async function copyText(text) {
  if (navigator.clipboard && navigator.clipboard.writeText) {
    await navigator.clipboard.writeText(text);
    return;
  }
  const textarea = document.createElement("textarea");
  textarea.value = text;
  textarea.style.position = "fixed";
  textarea.style.opacity = "0";
  document.body.appendChild(textarea);
  textarea.select();
  document.execCommand("copy");
  textarea.remove();
}

export function initClipboard() {
  document.addEventListener("click", async (event) => {
    const trigger = event.target.closest("[data-copy-text], [data-copy-target]");
    if (!trigger) return;
    event.preventDefault();
    const targetSelector = trigger.getAttribute("data-copy-target");
    const source = targetSelector ? document.querySelector(targetSelector) : null;
    const text = trigger.getAttribute("data-copy-text") || (source && source.textContent) || "";
    const analyticsSource = trigger.getAttribute("data-analytics-source");
    if (analyticsSource) track("cli_install_click", { source: analyticsSource });
    try {
      await copyText(text);
      updateBadge(trigger);
    } catch {
      updateBadge(trigger, "Copy failed");
    }
  });
}

function updateBadge(trigger, text = "Copied") {
  const badge = trigger.querySelector("[data-copy-badge], .hero-jump-copy");
  if (!badge) return;
  const previous = badge.textContent;
  badge.textContent = text;
  badge.classList.add("copied");
  window.setTimeout(() => {
    badge.textContent = previous;
    badge.classList.remove("copied");
  }, 1200);
}

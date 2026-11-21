import { iconMarkup } from "./icons.js";

export function showToast(message, isError = false) {
  const existing = document.querySelector(".toast");
  if (existing) existing.remove();

  const toast = document.createElement("div");
  toast.className = "toast";
  const icon = document.createElement("span");
  icon.className = "toast-icon";
  icon.innerHTML = iconMarkup(isError ? "x" : "check");
  const text = document.createElement("span");
  text.textContent = message;
  toast.append(icon, text);
  document.body.appendChild(toast);

  window.setTimeout(() => {
    toast.classList.add("closing");
    window.setTimeout(() => toast.remove(), 200);
  }, 3200);
}

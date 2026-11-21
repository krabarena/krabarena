// Space Invaders reskin — arcade "insert agent" copy toast.
//
// The hero quick-start command (`.hero-jump-cmd`, carrying `data-copy-text`)
// is copied to the clipboard by the bundled clipboard module. This file only
// adds the arcade flourish: a bottom-right pixel toast that flashes
// "CLAIM COPIED ► FEED IT TO YOUR AGENT" for ~1.8s on copy, matching the
// design prototype. Kept in /static (CSP script-src 'self') and decoupled
// from the copy logic so it degrades gracefully if either side is absent.
(function () {
  if (window.__krabArcadeToastInstalled) return;
  window.__krabArcadeToastInstalled = true;

  var TOAST_MS = 1800;
  var el = null;
  var hideTimer = null;

  function ensureEl() {
    if (el) return el;
    el = document.createElement("div");
    el.className = "ka-toast";
    el.setAttribute("role", "status");
    el.setAttribute("aria-live", "polite");
    el.textContent = "CLAIM COPIED ► FEED IT TO YOUR AGENT";
    document.body.appendChild(el);
    return el;
  }

  function showToast() {
    var node = ensureEl();
    node.classList.remove("is-hiding");
    // Restart the entry animation on repeat copies.
    node.classList.remove("is-visible");
    // Force reflow so the animation re-runs.
    void node.offsetWidth;
    node.classList.add("is-visible");
    if (hideTimer) clearTimeout(hideTimer);
    hideTimer = setTimeout(function () {
      node.classList.remove("is-visible");
      node.classList.add("is-hiding");
    }, TOAST_MS);
  }

  document.addEventListener("click", function (event) {
    var trigger = event.target.closest("[data-copy-text]");
    if (!trigger) return;
    // Only the hero "insert agent" command box gets the arcade toast.
    if (!trigger.closest(".hero-jump")) return;
    showToast();
  });
})();

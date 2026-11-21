// Unobtrusive toast for rate-limit auto-retry — since 0.26.0.
//
// Listens for the `krab:rate-limited` CustomEvent that fetch-retry.js
// dispatches on `window` before each retry wait, and shows a small
// transient pill so the user understands why a click is taking a moment
// instead of staring at a frozen-looking UI. The toast self-hides a
// short beat after the limiter's wait elapses; if another retry fires
// first, the timer is refreshed. When the request finally succeeds no
// further events fire, so the toast fades on its own.
//
// Deliberately decoupled from the fetch wrapper (separate file, only an
// event listener) so the wrapper stays DOM-free and unit-testable. All
// styling is inline so it needs no CSS-file changes and renders the same
// under either theme; CSP `script-src 'self'` already covers it.
(function () {
  if (window.__krabRateLimitToastInstalled) return;
  window.__krabRateLimitToastInstalled = true;

  var HIDE_BUFFER_MS = 1200; // linger past the wait so it isn't a flash
  var FADE_MS = 200;
  var el = null;
  var hideTimer = null;
  var removeTimer = null;

  function ensureEl() {
    if (el) return el;
    el = document.createElement('div');
    el.setAttribute('role', 'status');
    el.setAttribute('aria-live', 'polite');
    el.textContent = 'Too many requests — retrying automatically…';
    var s = el.style;
    s.position = 'fixed';
    s.left = '50%';
    s.bottom = '24px';
    s.transform = 'translateX(-50%) translateY(8px)';
    s.zIndex = '2147483647';
    s.maxWidth = 'calc(100vw - 32px)';
    s.padding = '10px 16px';
    s.borderRadius = '999px';
    s.background = 'rgba(24, 24, 27, 0.94)';
    s.color = '#fafafa';
    s.font =
      '500 14px/1.3 ui-sans-serif, system-ui, -apple-system, "Inter", sans-serif';
    s.boxShadow = '0 6px 24px rgba(0, 0, 0, 0.28)';
    s.border = '1px solid rgba(255, 255, 255, 0.08)';
    s.pointerEvents = 'none';
    s.opacity = '0';
    s.transition = 'opacity ' + FADE_MS + 'ms ease, transform ' + FADE_MS + 'ms ease';
    document.body.appendChild(el);
    return el;
  }

  function show(waitMs) {
    var node = ensureEl();
    if (removeTimer) {
      clearTimeout(removeTimer);
      removeTimer = null;
    }
    // Force a reflow-free async paint so the opacity transition runs even
    // on the first show right after append.
    node.style.display = 'block';
    requestAnimationFrame(function () {
      node.style.opacity = '1';
      node.style.transform = 'translateX(-50%) translateY(0)';
    });

    if (hideTimer) clearTimeout(hideTimer);
    var lingerMs = Math.max(0, (waitMs || 0)) + HIDE_BUFFER_MS;
    hideTimer = setTimeout(hide, lingerMs);
  }

  function hide() {
    if (!el) return;
    el.style.opacity = '0';
    el.style.transform = 'translateX(-50%) translateY(8px)';
    // Remove from layout after the fade so it can't catch clicks/AT focus.
    removeTimer = setTimeout(function () {
      if (el) el.style.display = 'none';
    }, FADE_MS + 20);
  }

  window.addEventListener('krab:rate-limited', function (e) {
    var waitMs = e && e.detail ? e.detail.waitMs : 0;
    show(waitMs);
  });
})();

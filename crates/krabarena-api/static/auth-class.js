// Pre-paint auth gate. Runs synchronously in <head> before the body paints
// so signed-in visitors never see a flash of the logged-out-only install
// CTA / Sign-in button on reload. Reads the JS-readable `ka_voter` presence
// cookie the server sets at login and marks <html class="ka-authed">; the
// `.ka-authed` CSS (in the bundle) hides those elements. nav.js still does
// the full auth toggle after load — this only removes the FOUC.
(function () {
  try {
    if (/(?:^|;\s*)ka_voter=1(?:;|$)/.test(document.cookie)) {
      document.documentElement.classList.add("ka-authed");
    }
  } catch (e) {
    /* cookies disabled / sandboxed — fall back to nav.js post-load toggle */
  }
})();

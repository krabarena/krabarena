// Auto-retry for rate-limited (HTTP 429) requests — since 0.26.0.
//
// The API rate limiter (web::rate_limit) returns 429 with a
// `Retry-After` header (seconds until the fixed window resets) when a
// client exceeds its budget. This wraps the global `fetch` so that a
// same-origin request that comes back 429 is automatically retried
// *after* the limiter says it'll be available again, instead of
// bubbling the error up to the page. A short over-limit burst then
// self-heals and the user sees the request eventually succeed.
//
// Scope + safety:
//   - Only *same-origin* requests are wrapped. Cross-origin calls
//     (Google Analytics / GTM, fonts) pass straight through to the
//     native fetch — we never retry someone else's 429.
//   - Only status 429 triggers a retry. Every other response (200, 4xx,
//     5xx) is returned to the caller untouched on the first try.
//   - Network errors (fetch rejects) are NOT retried — they propagate
//     as before, so callers' existing `.catch` paths are unchanged.
//   - Bounded: at most MAX_RETRIES attempts, and each wait is clamped to
//     MAX_WAIT_MS so a bogus/huge `Retry-After` can't hang the tab.
//   - Idempotent: installs once (guards re-injection) and preserves the
//     native fetch's `this` binding.
//
// Pages that want to react (e.g. show a "slow down, retrying…" hint)
// can listen for the `krab:rate-limited` CustomEvent dispatched on
// `window` before each wait; `event.detail` carries `{ url, waitMs,
// attempt, maxRetries }`.
(function () {
  if (window.__krabFetchRetryInstalled) return;
  if (typeof window.fetch !== 'function') return; // very old browser — leave as-is
  window.__krabFetchRetryInstalled = true;

  var nativeFetch = window.fetch;
  var MAX_RETRIES = 3; // initial attempt + up to 3 retries
  var MAX_WAIT_MS = 30000; // never sleep longer than this per retry
  var DEFAULT_WAIT_MS = 1000; // when Retry-After is missing/unparseable

  function requestUrl(input) {
    // `input` may be a string, URL, or Request object.
    if (input && typeof input === 'object' && 'url' in input) return input.url;
    return String(input);
  }

  function isSameOrigin(input) {
    try {
      return new URL(requestUrl(input), window.location.href).origin === window.location.origin;
    } catch (e) {
      // Unparseable → treat as same-origin (relative path) so we still
      // protect it; the worst case is one needless retry on a 429.
      return true;
    }
  }

  // Seconds → ms, preferring Retry-After, then RateLimit-Reset, then a
  // small default. Both headers are integer seconds from our limiter.
  function waitMsFrom(resp) {
    var secs = NaN;
    if (resp.headers) {
      var ra = resp.headers.get('retry-after');
      secs = ra ? parseInt(ra, 10) : NaN;
      if (!(isFinite(secs) && secs >= 0)) {
        var reset = resp.headers.get('ratelimit-reset');
        secs = reset ? parseInt(reset, 10) : NaN;
      }
    }
    var ms = isFinite(secs) && secs >= 0 ? secs * 1000 : DEFAULT_WAIT_MS;
    // A 0s Retry-After (window boundary) shouldn't hot-loop — floor it.
    return Math.max(250, Math.min(ms, MAX_WAIT_MS));
  }

  function sleep(ms) {
    return new Promise(function (resolve) {
      setTimeout(resolve, ms);
    });
  }

  window.fetch = function (input, init) {
    if (!isSameOrigin(input)) {
      return nativeFetch.call(this, input, init);
    }
    var self = this;
    var attempt = 0;

    function tryOnce() {
      return nativeFetch.call(self, input, init).then(function (resp) {
        if (resp.status !== 429 || attempt >= MAX_RETRIES) {
          return resp;
        }
        attempt++;
        var waitMs = waitMsFrom(resp);
        try {
          window.dispatchEvent(
            new CustomEvent('krab:rate-limited', {
              detail: {
                url: requestUrl(input),
                waitMs: waitMs,
                attempt: attempt,
                maxRetries: MAX_RETRIES,
              },
            })
          );
        } catch (e) {
          /* CustomEvent unsupported — ignore */
        }
        if (window.console && console.debug) {
          console.debug(
            '[krab] rate limited (429); retrying in ' +
              waitMs +
              'ms (attempt ' +
              attempt +
              '/' +
              MAX_RETRIES +
              ')'
          );
        }
        return sleep(waitMs).then(tryOnce);
      });
    }

    return tryOnce();
  };
})();

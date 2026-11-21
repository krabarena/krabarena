(function(window, document) {
  'use strict';

  var DEDUPE_WINDOW_MS = 1500;
  var MAX_QUEUE_LENGTH = 50;
  var MAX_STRING_LENGTH = 120;
  var UUID_RE = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;
  var SAFE_TOKEN_RE = /^[a-z0-9][a-z0-9_-]{0,119}$/i;
  var queue = [];
  var flushTimer = null;
  var recentEvents = {};

  var EVENT_CONTRACTS = {
    login_start: {
      params: {
        method: enumParam(['email', 'github', 'google', 'oauth'])
      }
    },
    login_success: {
      params: {
        method: enumParam(['email', 'github', 'google', 'oauth'])
      }
    },
    battle_open: {
      params: {
        battle_id: idParam
      }
    },
    claim_open: {
      params: {
        claim_id: idParam
      }
    },
    cli_install_click: {
      params: {
        source: enumParam(['home', 'about', 'battle', 'docs', 'unknown'])
      }
    }
  };

  var RESERVED_OR_SENSITIVE_KEYS = {
    email: true,
    token: true,
    voter_token: true,
    jwt: true,
    name: true,
    display_name: true,
    username: true,
    user_name: true,
    password: true,
    secret: true,
    code: true,
    claim: true,
    title: true,
    description: true,
    body: true,
    text: true
  };

  function enumParam(allowedValues) {
    var allowed = {};
    allowedValues.forEach(function(value) {
      allowed[value] = true;
    });
    return function(value) {
      value = normalizeToken(value);
      return allowed[value] ? value : null;
    };
  }

  function idParam(value) {
    value = normalizeString(value);
    if (!value) return null;
    if (UUID_RE.test(value)) return value.toLowerCase();
    if (SAFE_TOKEN_RE.test(value)) return value;
    return null;
  }

  function normalizeString(value) {
    if (value === undefined || value === null) return null;
    if (typeof value !== 'string' && typeof value !== 'number' && typeof value !== 'boolean') {
      return null;
    }
    value = String(value)
      .replace(/[\u0000-\u001f\u007f]/g, ' ')
      .replace(/\s+/g, ' ')
      .trim();
    if (!value) return null;
    return value.slice(0, MAX_STRING_LENGTH);
  }

  function normalizeToken(value) {
    value = normalizeString(value);
    return value ? value.toLowerCase() : null;
  }

  function isSensitiveKey(key) {
    key = String(key || '').toLowerCase();
    return RESERVED_OR_SENSITIVE_KEYS[key] === true;
  }

  function currentPath() {
    return safePath(window.location.pathname);
  }

  function pageLocation() {
    return window.location.origin + currentPath();
  }

  function pageTitle() {
    var meta = document.querySelector('meta[name="krab-analytics-title"]');
    var title = meta && meta.getAttribute('content');
    title = normalizeString(title);
    if (title) return title;
    title = normalizeString(document.title);
    if (title) return title;
    return 'KrabArena';
  }

  function safePath(path) {
    path = normalizeString(path) || '/';
    path = path.split('?')[0].split('#')[0];
    if (path === '/claims/standalone') return '/claims/standalone';
    if (path.indexOf('/battles/') === 0) return '/battles/:battle';
    if (path.indexOf('/claims/') === 0) return '/claims/:claim';
    if (path.indexOf('/agents/') === 0) return '/agents/:agent';
    if (path.indexOf('/docs/') === 0) return '/docs/:doc';
    if (path.indexOf('/login/callback') === 0) return '/login/callback';
    return path;
  }

  function commonParams() {
    return {
      page_location: pageLocation(),
      page_title: pageTitle(),
      page_path: currentPath()
    };
  }

  function normalizeParams(eventName, params) {
    var contract = EVENT_CONTRACTS[eventName];
    var normalized = commonParams();
    var schema = contract.params || {};

    params = params || {};
    Object.keys(schema).forEach(function(key) {
      if (isSensitiveKey(key)) return;
      var parser = schema[key];
      var value = parser(params[key]);
      if (value !== null) normalized[key] = value;
    });

    return normalized;
  }

  function canSend() {
    return typeof window.gtag === 'function';
  }

  function debugEnabled() {
    try {
      return window.location.search.indexOf('analytics_debug=1') !== -1 ||
        window.localStorage.getItem('krab_analytics_debug') === '1';
    } catch (e) {
      return false;
    }
  }

  function debugLog(message, payload) {
    if (!debugEnabled() || !window.console || typeof window.console.info !== 'function') return;
    window.console.info('[krabAnalytics] ' + message, payload || '');
  }

  function eventKey(eventName, payload) {
    return eventName + ':' + JSON.stringify(payload);
  }

  function isDuplicate(key) {
    var now = Date.now();
    var lastSeenAt = recentEvents[key] || 0;
    recentEvents[key] = now;

    Object.keys(recentEvents).forEach(function(existingKey) {
      if (now - recentEvents[existingKey] > DEDUPE_WINDOW_MS) {
        delete recentEvents[existingKey];
      }
    });

    return now - lastSeenAt < DEDUPE_WINDOW_MS;
  }

  function scheduleFlush() {
    if (flushTimer !== null) return;
    flushTimer = window.setTimeout(flushQueue, 0);
  }

  function flushQueue() {
    var pending;

    if (flushTimer !== null) {
      window.clearTimeout(flushTimer);
      flushTimer = null;
    }

    if (!queue.length) return;

    if (!canSend()) {
      debugLog('gtag unavailable, dropping queued events', { count: queue.length });
      queue = [];
      return;
    }

    pending = queue.slice();
    queue = [];

    pending.forEach(function(item) {
      try {
        window.gtag('event', item.eventName, item.payload);
        debugLog('event sent: ' + item.eventName, item.payload);
      } catch (e) {
        debugLog('event failed: ' + item.eventName, e);
      }
    });
  }

  function track(eventName, params) {
    try {
      eventName = normalizeToken(eventName);
      if (!eventName || !EVENT_CONTRACTS[eventName]) {
        debugLog('blocked unknown event', eventName);
        return false;
      }

      var payload = normalizeParams(eventName, params);
      var key = eventKey(eventName, payload);

      if (isDuplicate(key)) {
        debugLog('blocked duplicate event: ' + eventName, payload);
        return false;
      }

      if (queue.length >= MAX_QUEUE_LENGTH) {
        queue.shift();
      }

      queue.push({
        eventName: eventName,
        payload: payload
      });
      scheduleFlush();
      return true;
    } catch (e) {
      debugLog('event enqueue failed', e);
      return false;
    }
  }

  function isEnabled() {
    return canSend();
  }

  window.krabAnalytics = Object.freeze({
    isEnabled: isEnabled,
    pageLocation: pageLocation,
    pagePath: currentPath,
    pageTitle: pageTitle,
    track: track
  });

  window.addEventListener('pagehide', flushQueue);
  window.addEventListener('beforeunload', flushQueue);
  document.addEventListener('visibilitychange', function() {
    if (document.visibilityState === 'hidden') {
      flushQueue();
    }
  });

  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', function() {
      debugLog('ready', { enabled: isEnabled(), path: currentPath() });
    });
  } else {
    debugLog('ready', { enabled: isEnabled(), path: currentPath() });
  }
})(window, document);

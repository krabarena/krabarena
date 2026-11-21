const AUTH_KEYS = Object.freeze({
  voterToken: "voter_token",
  voterName: "voter_name",
  adminToken: "krabarena_token",
  loginReturn: "login_return",
  loginMethod: "login_method",
  analyticsDebug: "krab_analytics_debug"
});

// JS-readable cookies set by the server. The session token itself lives in
// the HttpOnly ka_session cookie and is invisible here by design; ka_voter
// is a presence flag ("1") so the UI can tell it's logged in, and ka_csrf
// is the double-submit token echoed on state-changing requests.
const VOTER_FLAG_COOKIE = "ka_voter";
const CSRF_COOKIE = "ka_csrf";

/** Read a single cookie value, or null. Safe if `document.cookie` throws. */
export function readCookie(name) {
  try {
    const target = `${name}=`;
    const parts =
      typeof document !== "undefined" && document.cookie ? document.cookie.split(";") : [];
    for (const part of parts) {
      const trimmed = part.trim();
      if (trimmed.startsWith(target)) return decodeURIComponent(trimmed.slice(target.length));
    }
  } catch {
    // sandboxed / disabled cookies — treat as absent.
  }
  return null;
}

/** The real voter session token from localStorage (legacy header auth). */
export function getVoterAuthToken() {
  return getSessionValue(AUTH_KEYS.voterToken);
}

/** The double-submit CSRF token for state-changing requests, or null. */
export function getCsrfToken() {
  return readCookie(CSRF_COOKIE);
}

/** True when signed in via the cookie flag OR a legacy localStorage token. */
export function isVoterLoggedIn() {
  return readCookie(VOTER_FLAG_COOKIE) === "1" || Boolean(getVoterAuthToken());
}

function storage() {
  try {
    return window.localStorage;
  } catch {
    return null;
  }
}

export function getSessionValue(key) {
  const store = storage();
  if (!store) return null;
  return store.getItem(key);
}

export function setSessionValue(key, value) {
  const store = storage();
  if (!store || value === undefined || value === null) return;
  store.setItem(key, String(value));
}

export function removeSessionValue(key) {
  const store = storage();
  if (!store) return;
  store.removeItem(key);
}

export function getAuthSession() {
  // `voterToken` here is a signed-in signal for UI gating: the real token
  // if we still have one in localStorage, otherwise a "1" sentinel when the
  // ka_voter cookie says we're logged in (post-migration, the token lives in
  // the HttpOnly cookie and is not readable). Header injectors that need the
  // actual token use getVoterAuthToken() instead.
  const storedVoter = getSessionValue(AUTH_KEYS.voterToken);
  const voterToken = storedVoter || (readCookie(VOTER_FLAG_COOKIE) === "1" ? "1" : null);
  return {
    voterToken,
    voterName: getSessionValue(AUTH_KEYS.voterName),
    adminToken: getSessionValue(AUTH_KEYS.adminToken)
  };
}

export function setAuthSession({ voterToken, voterName, adminToken }) {
  if (voterToken) setSessionValue(AUTH_KEYS.voterToken, voterToken);
  if (voterName) setSessionValue(AUTH_KEYS.voterName, voterName);
  if (adminToken) setSessionValue(AUTH_KEYS.adminToken, adminToken);
}

export function clearAuthSession() {
  clearVoterSession();
  removeSessionValue(AUTH_KEYS.adminToken);
}

export function clearVoterSession() {
  removeSessionValue(AUTH_KEYS.voterToken);
  removeSessionValue(AUTH_KEYS.voterName);
}

export function setLoginReturn(value) {
  setSessionValue(AUTH_KEYS.loginReturn, value);
}

export function takeLoginReturn(fallback = "/") {
  const value = getSessionValue(AUTH_KEYS.loginReturn) || fallback;
  removeSessionValue(AUTH_KEYS.loginReturn);
  return value;
}

export function setLoginMethod(method) {
  if (method) setSessionValue(AUTH_KEYS.loginMethod, method);
}

export function takeLoginMethod(fallback = "oauth") {
  const value = getSessionValue(AUTH_KEYS.loginMethod) || fallback;
  removeSessionValue(AUTH_KEYS.loginMethod);
  return value;
}

export function analyticsDebugEnabled() {
  return (
    window.location.search.includes("analytics_debug=1") ||
    getSessionValue(AUTH_KEYS.analyticsDebug) === "1"
  );
}

export { AUTH_KEYS };

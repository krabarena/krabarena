import { getAuthSession, getCsrfToken, getVoterAuthToken } from "./session.js";

export function initHtmxAuth() {
  document.body.addEventListener("htmx:configRequest", (event) => {
    const { adminToken } = getAuthSession();
    const voterToken = getVoterAuthToken();
    if (voterToken) event.detail.headers["x-voter-token"] = voterToken;
    if (adminToken) event.detail.headers.Authorization = `Bearer ${adminToken}`;
    // Cookie-authed mutations need the double-submit CSRF token.
    const verb = String(event.detail.verb || "get").toUpperCase();
    if (verb !== "GET" && verb !== "HEAD") {
      const csrf = getCsrfToken();
      if (csrf) event.detail.headers["x-csrf-token"] = csrf;
    }
  });
}

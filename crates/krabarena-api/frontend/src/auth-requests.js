import { create } from "axios";

import { getCsrfToken, getVoterAuthToken } from "./session.js";

const JSON_ACCEPT = "application/json";
const DEFAULT_VOTER_HEADER = "x-voter-token";

export function getVoterToken() {
  return getVoterAuthToken();
}

export function createKrabApiClient({ adapter, readVoterToken = getVoterToken } = {}) {
  const client = create({
    adapter,
    baseURL: "/",
    // Send the ka_session / ka_device / ka_csrf cookies. Same-origin so
    // this is effectively the default, but explicit for clarity.
    withCredentials: true,
    headers: {
      Accept: JSON_ACCEPT
    },
    validateStatus: () => true
  });

  client.interceptors.request.use((config) => {
    // Double-submit CSRF token on state-changing requests (the server
    // enforces it for cookie-authed mutations).
    const method = String(config.method || "get").toUpperCase();
    if (method !== "GET" && method !== "HEAD") {
      const csrf = getCsrfToken();
      if (csrf) setHeader(config, "x-csrf-token", csrf);
    }
    // Legacy header auth, still sent during the migration window (and for
    // non-cookie callers). Harmless once migrated — the cookie wins.
    if (config.krabAuth === "voter") {
      const token = readVoterToken();
      if (token) setHeader(config, DEFAULT_VOTER_HEADER, token);
    }
    return config;
  });

  return client;
}

export const apiClient = createKrabApiClient();

export async function requestJson(config, { client = apiClient } = {}) {
  return normalizeResponse(await client.request(config));
}

export function getJson(url, config = {}, options = {}) {
  return requestJson({ ...config, method: "GET", url }, options);
}

export function postJson(url, data, config = {}, options = {}) {
  return requestJson({ ...config, data, method: "POST", url }, options);
}

export function putJson(url, data, config = {}, options = {}) {
  return requestJson({ ...config, data, method: "PUT", url }, options);
}

function setHeader(config, name, value) {
  if (typeof config.headers?.set === "function") {
    config.headers.set(name, value);
    return;
  }
  config.headers = { ...(config.headers || {}), [name]: value };
}

function normalizeResponse(response) {
  return {
    data: response.data,
    headers: response.headers,
    ok: response.status >= 200 && response.status < 300,
    response,
    status: response.status
  };
}

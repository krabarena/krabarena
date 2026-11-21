import assert from "node:assert/strict";
import { beforeEach, test } from "node:test";

import { createKrabApiClient, getJson, postJson } from "./auth-requests.js";
import { clearAuthSession, setAuthSession } from "./session.js";

function makeStorage() {
  const values = new Map();
  return {
    getItem(key) {
      return values.has(key) ? values.get(key) : null;
    },
    removeItem(key) {
      values.delete(key);
    },
    setItem(key, value) {
      values.set(key, String(value));
    }
  };
}

beforeEach(() => {
  global.window = {
    localStorage: makeStorage(),
    location: { search: "" }
  };
  global.document = { cookie: "" };
});

test("Axios client adds the voter token from session storage for voter-auth requests", async () => {
  setAuthSession({ voterToken: "voter-123" });
  const calls = [];
  const client = createKrabApiClient({ adapter: capture(calls) });

  const response = await getJson("/v1/profile/me", { krabAuth: "voter" }, { client });

  assert.equal(response.ok, true);
  assert.equal(header(calls[0], "accept"), "application/json");
  assert.equal(header(calls[0], "x-voter-token"), "voter-123");
});

test("Axios client omits voter auth headers when session has no voter token", async () => {
  clearAuthSession();
  const calls = [];
  const client = createKrabApiClient({ adapter: capture(calls) });

  await getJson("/v1/profile/me", { krabAuth: "voter" }, { client });

  assert.equal(header(calls[0], "x-voter-token"), null);
});

test("Axios client does not attach voter tokens to public JSON requests", async () => {
  setAuthSession({ voterToken: "voter-123" });
  const calls = [];
  const client = createKrabApiClient({ adapter: capture(calls) });

  await postJson("/auth/voter/email", { email: "a@example.test" }, {}, { client });

  assert.equal(header(calls[0], "accept"), "application/json");
  assert.equal(header(calls[0], "x-voter-token"), null);
});

test("Axios client attaches the CSRF token on mutating requests when the cookie is present", async () => {
  global.document.cookie = "ka_csrf=csrf-abc";
  const calls = [];
  const client = createKrabApiClient({ adapter: capture(calls) });

  await postJson("/v1/votes", { choice: "a" }, { krabAuth: "voter" }, { client });

  assert.equal(header(calls[0], "x-csrf-token"), "csrf-abc");
});

test("Axios client omits the CSRF token on GET requests", async () => {
  global.document.cookie = "ka_csrf=csrf-abc";
  const calls = [];
  const client = createKrabApiClient({ adapter: capture(calls) });

  await getJson("/v1/profile/me", { krabAuth: "voter" }, { client });

  assert.equal(header(calls[0], "x-csrf-token"), null);
});

test("Axios client sends cookies with credentials", () => {
  const client = createKrabApiClient({ adapter: capture([]) });
  assert.equal(client.defaults.withCredentials, true);
});

test("Axios JSON helper preserves non-2xx responses for caller handling", async () => {
  const client = createKrabApiClient({
    adapter: capture([], {
      data: { error: { message: "Invalid code." } },
      status: 422
    })
  });

  const response = await postJson("/auth/voter/email/verify", { code: "bad" }, {}, { client });

  assert.equal(response.ok, false);
  assert.equal(response.status, 422);
  assert.equal(response.data.error.message, "Invalid code.");
});

test("Axios JSON helper passes query params through config objects", async () => {
  const calls = [];
  const client = createKrabApiClient({ adapter: capture(calls) });

  await getJson("/v1/profile/agent-notifications", { params: { limit: 50 } }, { client });

  assert.deepEqual(calls[0].params, { limit: 50 });
});

function capture(calls, response = {}) {
  return async (config) => {
    calls.push(config);
    return {
      config,
      data: response.data ?? { data: { ok: true } },
      headers: response.headers ?? {},
      request: {},
      status: response.status ?? 200,
      statusText: response.statusText ?? "OK"
    };
  };
}

function header(config, name) {
  const target = name.toLowerCase();
  const headers =
    typeof config.headers?.toJSON === "function" ? config.headers.toJSON() : config.headers || {};
  const match = Object.entries(headers).find(([key]) => key.toLowerCase() === target);
  return match ? match[1] : null;
}

import { test, beforeEach, afterEach } from "node:test";
import assert from "node:assert/strict";

import { getCaptchaToken, readOAuthCallbackParams } from "./login.js";

// getCaptchaToken reaches for window.setTimeout/clearTimeout for its execute
// guard. Stub them (the injected fake clock drives the wait loop, so these
// timers never actually fire in the happy paths).
const realWindow = global.window;
beforeEach(() => {
  global.window = { setTimeout: () => 1, clearTimeout: () => {} };
});
afterEach(() => {
  global.window = realWindow;
});

test("disabled: no site key resolves to null without touching grecaptcha", async () => {
  let apiCalls = 0;
  const token = await getCaptchaToken({
    siteKey: null,
    getApi: () => {
      apiCalls += 1;
      return {};
    }
  });
  assert.equal(token, null);
  assert.equal(apiCalls, 0, "should short-circuit before probing grecaptcha");
});

test("token available immediately resolves with the token", async () => {
  const api = {
    enterprise: { ready: (cb) => cb(), execute: async () => "tok-now" }
  };
  const token = await getCaptchaToken({
    siteKey: "site",
    getApi: () => api,
    now: () => 0,
    sleep: async () => {}
  });
  assert.equal(token, "tok-now");
});

test("waits for the async-defer script, then resolves the token", async () => {
  // grecaptcha.enterprise appears only on the 3rd probe (script still loading).
  let calls = 0;
  const api = {
    enterprise: { ready: (cb) => cb(), execute: async () => "tok-late" }
  };
  const getApi = () => (++calls >= 3 ? api : {});

  let clock = 0;
  const slept = [];
  const token = await getCaptchaToken({
    siteKey: "site",
    getApi,
    now: () => clock,
    sleep: async (ms) => {
      slept.push(ms);
      clock += ms;
    },
    timeoutMs: 30000,
    pollMs: 100
  });

  assert.equal(token, "tok-late");
  assert.deepEqual(slept, [100, 100], "polled twice before the script was ready");
});

test("script never loads within the timeout resolves to null", async () => {
  let clock = 0;
  const token = await getCaptchaToken({
    siteKey: "site",
    getApi: () => ({}), // enterprise never appears
    now: () => clock,
    sleep: async (ms) => {
      clock += ms;
    },
    timeoutMs: 500,
    pollMs: 100
  });
  assert.equal(token, null);
});

test("execute() rejection resolves to null (never wedges send)", async () => {
  const api = {
    enterprise: {
      ready: (cb) => cb(),
      execute: async () => {
        throw new Error("challenge dismissed");
      }
    }
  };
  const token = await getCaptchaToken({
    siteKey: "site",
    getApi: () => api,
    now: () => 0,
    sleep: async () => {}
  });
  assert.equal(token, null);
});

// Regression: the OAuth callback used to put the voter session token in the
// query string (`/login/callback?token=…`), leaking it into server/proxy logs
// and Referer headers. It now travels in the fragment only.
test("oauth callback reads the token from the fragment, not the query", () => {
  assert.deepEqual(readOAuthCallbackParams({ hash: "#token=abc&name=Ann", search: "" }), {
    token: "abc",
    name: "Ann"
  });
  assert.equal(readOAuthCallbackParams({ hash: "", search: "?token=leaked" }).token, null);
  assert.equal(readOAuthCallbackParams({}).token, null);
});

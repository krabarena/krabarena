import assert from "node:assert/strict";
import { beforeEach, test } from "node:test";

import {
  AUTH_KEYS,
  clearAuthSession,
  clearVoterSession,
  getAuthSession,
  getCsrfToken,
  getVoterAuthToken,
  isVoterLoggedIn,
  readCookie,
  setAuthSession,
  setLoginMethod,
  setLoginReturn,
  takeLoginMethod,
  takeLoginReturn
} from "./session.js";

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

test("auth session is stored and cleared through the session module", () => {
  setAuthSession({
    voterToken: "voter-token",
    voterName: "Ada",
    adminToken: "admin-token"
  });

  assert.deepEqual(getAuthSession(), {
    voterToken: "voter-token",
    voterName: "Ada",
    adminToken: "admin-token"
  });

  clearAuthSession();
  assert.deepEqual(getAuthSession(), {
    voterToken: null,
    voterName: null,
    adminToken: null
  });
});

test("voter session can be cleared without clearing admin session", () => {
  setAuthSession({
    voterToken: "stale-voter-token",
    voterName: "Ada",
    adminToken: "admin-token"
  });

  clearVoterSession();
  assert.deepEqual(getAuthSession(), {
    voterToken: null,
    voterName: null,
    adminToken: "admin-token"
  });
});

test("readCookie parses a named cookie and returns null when absent", () => {
  global.document.cookie = "ka_device=abc; ka_voter=1; ka_csrf=tok-123";
  assert.equal(readCookie("ka_voter"), "1");
  assert.equal(readCookie("ka_csrf"), "tok-123");
  assert.equal(readCookie("ka_device"), "abc");
  assert.equal(readCookie("missing"), null);
});

test("getCsrfToken reads the ka_csrf cookie", () => {
  global.document.cookie = "ka_csrf=csrf-value";
  assert.equal(getCsrfToken(), "csrf-value");
});

test("isVoterLoggedIn is true via the ka_voter cookie flag with no localStorage token", () => {
  global.document.cookie = "ka_voter=1";
  assert.equal(getVoterAuthToken(), null);
  assert.equal(isVoterLoggedIn(), true);
});

test("getAuthSession yields a signed-in sentinel from the cookie when localStorage has no token", () => {
  global.document.cookie = "ka_voter=1";
  setAuthSession({ voterName: "Ada" });
  assert.deepEqual(getAuthSession(), {
    voterToken: "1",
    voterName: "Ada",
    adminToken: null
  });
});

test("getAuthSession prefers the real localStorage token over the cookie sentinel", () => {
  global.document.cookie = "ka_voter=1";
  setAuthSession({ voterToken: "real-token" });
  assert.equal(getAuthSession().voterToken, "real-token");
  assert.equal(getVoterAuthToken(), "real-token");
});

test("login return and method are consumed once", () => {
  setLoginReturn("/battles/demo");
  setLoginMethod("github");

  assert.equal(takeLoginReturn("/"), "/battles/demo");
  assert.equal(takeLoginMethod("oauth"), "github");
  assert.equal(takeLoginReturn("/"), "/");
  assert.equal(takeLoginMethod("oauth"), "oauth");
  assert.equal(global.window.localStorage.getItem(AUTH_KEYS.loginReturn), null);
  assert.equal(global.window.localStorage.getItem(AUTH_KEYS.loginMethod), null);
});

import assert from "node:assert/strict";
import { beforeEach, test } from "node:test";

import { migrateLegacySession } from "./migrate.js";
import { AUTH_KEYS } from "./session.js";

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
  global.window = { localStorage: makeStorage(), location: { search: "" } };
  global.document = { cookie: "" };
});

test("migration is a no-op when already on cookies", async () => {
  global.document.cookie = "ka_voter=1";
  global.window.localStorage.setItem(AUTH_KEYS.voterToken, "legacy-token");
  let called = false;
  const request = async () => {
    called = true;
    return { ok: true };
  };

  assert.equal(await migrateLegacySession({ request }), "already-migrated");
  assert.equal(called, false);
});

test("migration is a no-op for anonymous visitors", async () => {
  let called = false;
  const request = async () => {
    called = true;
    return { ok: true };
  };

  assert.equal(await migrateLegacySession({ request }), "no-token");
  assert.equal(called, false);
});

test("migration posts the legacy token then clears localStorage on success", async () => {
  global.window.localStorage.setItem(AUTH_KEYS.voterToken, "legacy-token");
  global.window.localStorage.setItem(AUTH_KEYS.voterName, "Ada");
  const calls = [];
  const request = async (url, body, config) => {
    calls.push({ url, body, config });
    return { ok: true };
  };

  assert.equal(await migrateLegacySession({ request }), "migrated");
  assert.equal(calls[0].url, "/auth/migrate");
  assert.equal(calls[0].config.krabAuth, "voter");
  // Token dropped, display name kept.
  assert.equal(global.window.localStorage.getItem(AUTH_KEYS.voterToken), null);
  assert.equal(global.window.localStorage.getItem(AUTH_KEYS.voterName), "Ada");
});

test("migration keeps the legacy token when the server rejects it", async () => {
  global.window.localStorage.setItem(AUTH_KEYS.voterToken, "legacy-token");
  const request = async () => ({ ok: false });

  assert.equal(await migrateLegacySession({ request }), "failed");
  assert.equal(global.window.localStorage.getItem(AUTH_KEYS.voterToken), "legacy-token");
});

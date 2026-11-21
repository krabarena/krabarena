import assert from "node:assert/strict";
import { test } from "node:test";

import { notificationBody, notificationHref, notificationLabel } from "./notification-format.js";

test("href uses the url path so links stay on the current host", () => {
  assert.equal(
    notificationHref({ url: "https://krabarena.com/battles/best-search-api", battle_id: "b1" }),
    "/battles/best-search-api"
  );
});

test("href falls back to claim id, then battle id", () => {
  assert.equal(notificationHref({ claim_id: "c1" }), "/claims/c1");
  assert.equal(notificationHref({ claim_id: null, battle_id: "b1" }), "/battles/b1");
  assert.equal(notificationHref({}), "#");
});

test("label falls back to kind for responses from an older server", () => {
  assert.equal(notificationLabel({ kind: "battle_claim", label: "New claim" }), "New claim");
  assert.equal(notificationLabel({ kind: "comment" }), "comment");
});

test("body prefers the server preview, then legacy fields", () => {
  assert.equal(notificationBody({ preview: "short", body: "long body" }), "short");
  assert.equal(notificationBody({ body: "a comment" }), "a comment");
  assert.equal(notificationBody({ failure_reason: "did not reproduce" }), "did not reproduce");
  assert.equal(notificationBody({ summary: "reviewer says ok" }), "reviewer says ok");
  assert.equal(notificationBody({}), "");
});

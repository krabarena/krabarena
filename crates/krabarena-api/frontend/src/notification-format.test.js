import assert from "node:assert/strict";
import { test } from "node:test";

import {
  notificationActor,
  notificationBody,
  notificationHref,
  notificationLabel,
  openNotification,
  unreadBadgeText
} from "./notification-format.js";

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

test("battle-level items link to the battle, never /claims/null", () => {
  const summary = {
    kind: "battle_summary",
    claim_id: null,
    battle_id: "b1",
    url: "https://krabarena.com/battles/best-fetch-api"
  };
  assert.equal(notificationHref(summary), "/battles/best-fetch-api");
  assert.equal(notificationHref({ ...summary, url: undefined }), "/battles/b1");
});

test("actor falls back to KrabReviewer only for actor-less items", () => {
  assert.equal(notificationActor({ author_id: "u1", author_name: "kesku" }), "kesku");
  assert.equal(notificationActor({ author_id: null }), "KrabReviewer");
  assert.equal(notificationActor({ author_id: "u1" }), "");
});

test("badge text shows the count, 99+ when capped, nothing when zero", () => {
  assert.equal(unreadBadgeText({ unread: 0 }), "");
  assert.equal(unreadBadgeText({ unread: 7 }), "7");
  assert.equal(unreadBadgeText({ unread: 99, capped: true }), "99+");
});

function clickEvent(overrides = {}) {
  return {
    prevented: false,
    preventDefault() {
      this.prevented = true;
    },
    button: 0,
    ...overrides
  };
}

test("plain click marks the item read, then navigates to its target", async () => {
  const marked = [];
  const visited = [];
  const item = { id: "n1", read: false, url: "https://krabarena.com/claims/c1" };
  const event = clickEvent();
  await openNotification(event, item, {
    markRead: async (id) => marked.push(id),
    navigate: (href) => visited.push(href)
  });
  assert.equal(event.prevented, true);
  assert.deepEqual(marked, ["n1"]);
  assert.deepEqual(visited, ["/claims/c1"]);
  assert.equal(item.read, true);
});

test("a slow mark-read does not block navigation", async () => {
  const visited = [];
  await openNotification(
    clickEvent(),
    { id: "n2", read: false, url: "https://krabarena.com/battles/b" },
    { markRead: () => new Promise(() => {}), navigate: (href) => visited.push(href), waitMs: 5 }
  );
  assert.deepEqual(visited, ["/battles/b"]);
});

test("modified click keeps the default (new tab) and still marks read", async () => {
  const marked = [];
  const event = clickEvent({ metaKey: true });
  await openNotification(
    event,
    { id: "n3", read: false, claim_id: "c3" },
    {
      markRead: async (id) => marked.push(id),
      navigate: () => assert.fail("must not navigate")
    }
  );
  assert.equal(event.prevented, false);
  assert.deepEqual(marked, ["n3"]);
});

test("already-read items are not marked again", async () => {
  let calls = 0;
  await openNotification(
    clickEvent(),
    { id: "n4", read: true, claim_id: "c4" },
    {
      markRead: async () => {
        calls += 1;
      },
      navigate: () => {}
    }
  );
  assert.equal(calls, 0);
});

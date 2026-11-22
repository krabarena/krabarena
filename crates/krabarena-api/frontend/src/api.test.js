import assert from "node:assert/strict";
import { beforeEach, test } from "node:test";

import {
  getAgentNotifications,
  logoutVoter,
  proposeBattleCandidate,
  sendVoterEmailCode,
  unvoteBattleCandidate,
  updateAgentProfile,
  verifyVoterEmailCode,
  voteBattleCandidate
} from "./api.js";
import { createKrabApiClient } from "./auth-requests.js";
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
  clearAuthSession();
});

test("getAgentNotifications sends params object and voter auth", async () => {
  setAuthSession({ voterToken: "voter-123" });
  const calls = [];
  const client = createKrabApiClient({ adapter: capture(calls) });

  await getAgentNotifications({ limit: 50 }, { client });

  assert.equal(calls[0].url, "/v1/profile/agent-notifications");
  assert.deepEqual(calls[0].params, { limit: 50 });
  assert.equal(header(calls[0], "x-voter-token"), "voter-123");
});

test("logoutVoter sends voter-authenticated logout request", async () => {
  setAuthSession({ voterToken: "voter-logout" });
  const calls = [];
  const client = createKrabApiClient({ adapter: capture(calls) });

  await logoutVoter({ client });

  assert.equal(calls[0].method, "post");
  assert.equal(calls[0].url, "/v1/profile/logout");
  assert.equal(header(calls[0], "x-voter-token"), "voter-logout");
});

test("candidate vote wrappers build existing vote paths", async () => {
  const calls = [];
  const client = createKrabApiClient({ adapter: capture(calls) });

  await voteBattleCandidate({ candidateId: "abc-123" }, { client });
  await unvoteBattleCandidate({ candidateId: "abc-123" }, { client });

  assert.equal(calls[0].url, "/v1/battle-candidates/abc-123/vote");
  assert.equal(calls[1].url, "/v1/battle-candidates/abc-123/unvote");
});

test("candidate proposal wrapper sends title and rationale body", async () => {
  const calls = [];
  const client = createKrabApiClient({ adapter: capture(calls) });

  await proposeBattleCandidate(
    { rationale: "Useful benchmark.", title: "Benchmark proposal" },
    { client }
  );

  assert.equal(calls[0].url, "/v1/battle-candidates");
  assert.deepEqual(requestData(calls[0]), {
    rationale: "Useful benchmark.",
    title: "Benchmark proposal"
  });
});

test("email wrappers send the existing JSON request bodies", async () => {
  const calls = [];
  const client = createKrabApiClient({ adapter: capture(calls) });

  await sendVoterEmailCode({ email: "agent@example.test" }, { client });
  await verifyVoterEmailCode({ code: "12345678", email: "agent@example.test" }, { client });

  assert.equal(calls[0].url, "/auth/voter/email");
  assert.deepEqual(requestData(calls[0]), { email: "agent@example.test" });
  assert.equal(calls[1].url, "/auth/voter/email/verify");
  assert.deepEqual(requestData(calls[1]), {
    code: "12345678",
    email: "agent@example.test"
  });
});

test("sendVoterEmailCode includes captcha_token only when provided", async () => {
  const calls = [];
  const client = createKrabApiClient({ adapter: capture(calls) });

  // Without a token (captcha disabled), the field is omitted entirely.
  await sendVoterEmailCode({ email: "a@example.test" }, { client });
  assert.deepEqual(requestData(calls[0]), { email: "a@example.test" });

  // With a token, it rides along as captcha_token.
  await sendVoterEmailCode({ captchaToken: "tok-123", email: "b@example.test" }, { client });
  assert.deepEqual(requestData(calls[1]), {
    captcha_token: "tok-123",
    email: "b@example.test"
  });
});

test("updateAgentProfile PATCHes only known profile fields with voter auth", async () => {
  setAuthSession({ voterToken: "voter-profile" });
  const calls = [];
  const client = createKrabApiClient({ adapter: capture(calls) });

  await updateAgentProfile(
    "user/1",
    { bio: null, evil: "x", full_name: "Jane Doe", github: "jane", x: "" },
    { client }
  );

  assert.equal(calls[0].method, "patch");
  assert.equal(calls[0].url, "/v1/agents/user%2F1/profile");
  assert.equal(header(calls[0], "x-voter-token"), "voter-profile");
  assert.deepEqual(requestData(calls[0]), {
    bio: "",
    full_name: "Jane Doe",
    github: "jane",
    x: ""
  });
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

function requestData(config) {
  return typeof config.data === "string" ? JSON.parse(config.data) : config.data;
}

import { getJson, postJson, putJson } from "./auth-requests.js";

const VOTER_AUTH = Object.freeze({ krabAuth: "voter" });

export function getCurrentVoter(options = {}) {
  return getJson("/auth/voter/me", VOTER_AUTH, options);
}

export function sendVoterEmailCode({ email, captchaToken }, options = {}) {
  // Omit captcha_token entirely when there is no token (captcha disabled
  // server-side, or the about-page send path that has no widget).
  const body = captchaToken ? { email, captcha_token: captchaToken } : { email };
  return postJson("/auth/voter/email", body, {}, options);
}

export function verifyVoterEmailCode({ email, code }, options = {}) {
  return postJson("/auth/voter/email/verify", { email, code }, {}, options);
}

export function sendDeviceEmailLink({ email }, options = {}) {
  return postJson("/v1/auth/email", { email }, {}, options);
}

export function getProfile(options = {}) {
  return getJson("/v1/profile/me", VOTER_AUTH, options);
}

export function getAgentNotifications({ limit = 50, cursor, unread } = {}, options = {}) {
  const params = { limit };
  if (cursor) params.cursor = cursor;
  if (unread) params.unread = true;
  return getJson("/v1/profile/agent-notifications", { ...VOTER_AUTH, params }, options);
}

export function getAgentNotificationsUnreadCount(options = {}) {
  return getJson("/v1/profile/agent-notifications/unread-count", VOTER_AUTH, options);
}

export function markAgentNotificationRead(id, options = {}) {
  return postJson(
    `/v1/profile/agent-notifications/${encodeURIComponent(id)}/read`,
    undefined,
    VOTER_AUTH,
    options
  );
}

export function markAllAgentNotificationsRead(options = {}) {
  return postJson("/v1/profile/agent-notifications/read-all", undefined, VOTER_AUTH, options);
}

export function logoutVoter(options = {}) {
  return postJson("/v1/profile/logout", undefined, VOTER_AUTH, options);
}

export function getWallet(options = {}) {
  return getJson("/v1/profile/wallet", VOTER_AUTH, options);
}

export function saveWallet({ address }, options = {}) {
  return putJson("/v1/profile/wallet", { address }, VOTER_AUTH, options);
}

export function getTransactions({ limit = 100 } = {}, options = {}) {
  return getJson("/v1/profile/transactions", { ...VOTER_AUTH, params: { limit } }, options);
}

export function searchSuggest({ q, kind = "all", ai = true } = {}, options = {}) {
  return getJson("/v1/search/suggest", { params: { q, kind, ai: ai ? 1 : 0 } }, options);
}

export function proposeBattleCandidate({ title, rationale }, options = {}) {
  return postJson("/v1/battle-candidates", { title, rationale }, VOTER_AUTH, options);
}

export function voteBattleCandidate({ candidateId }, options = {}) {
  return postJson(`/v1/battle-candidates/${candidateId}/vote`, undefined, VOTER_AUTH, options);
}

export function unvoteBattleCandidate({ candidateId }, options = {}) {
  return postJson(`/v1/battle-candidates/${candidateId}/unvote`, undefined, VOTER_AUTH, options);
}

export function castClaimVote({ claimId, voteType }, options = {}) {
  return postJson(
    `/v1/claims/${encodeURIComponent(claimId)}/votes`,
    { vote_type: voteType },
    VOTER_AUTH,
    options
  );
}

export function getClaimVotes({ claimId }, options = {}) {
  return getJson(`/v1/claims/${encodeURIComponent(claimId)}/votes`, {}, options);
}

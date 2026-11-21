import { postJson } from "./auth-requests.js";
import { AUTH_KEYS, getSessionValue, readCookie, removeSessionValue } from "./session.js";

// One-shot upgrade of a legacy localStorage voter session to the cookie
// session. Runs on every load but no-ops once migrated (or for anonymous
// visitors). The server reads the legacy x-voter-token header and responds
// with Set-Cookie for ka_session/ka_csrf/ka_voter; we then drop the
// localStorage token so the HttpOnly cookie is the single source of truth.
// The display name (voter_name) is kept for the account avatar.
//
// Returns a status string (useful for tests); the caller fires and forgets.
export async function migrateLegacySession({ request = postJson } = {}) {
  if (readCookie("ka_voter") === "1") return "already-migrated";
  const token = getSessionValue(AUTH_KEYS.voterToken);
  if (!token) return "no-token";

  try {
    const res = await request("/auth/migrate", {}, { krabAuth: "voter" });
    if (res.ok) {
      removeSessionValue(AUTH_KEYS.voterToken);
      return "migrated";
    }
    return "failed";
  } catch {
    // Non-fatal: legacy header auth keeps working until the next load.
    return "error";
  }
}

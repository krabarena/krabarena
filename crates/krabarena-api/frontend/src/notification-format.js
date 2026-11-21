// Pure display helpers for `/v1/profile/agent-notifications` items.
// Kept DOM-free so `node --test` can cover them.

/** Relative link to the item's target Claim or Battle. */
export function notificationHref(item) {
  if (item.url) {
    try {
      return new URL(item.url).pathname;
    } catch {
      // fall through to the id-based link
    }
  }
  if (item.claim_id) return `/claims/${item.claim_id}`;
  if (item.battle_id) return `/battles/${item.battle_id}`;
  return "#";
}

/** Short kind label ("Verified", "New claim", …); falls back to the raw kind. */
export function notificationLabel(item) {
  return item.label || item.kind || "";
}

/** One-line body text: the server preview, else the legacy fields. */
export function notificationBody(item) {
  return item.preview || item.body || item.failure_reason || item.summary || "";
}

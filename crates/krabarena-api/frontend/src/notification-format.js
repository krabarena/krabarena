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

/** Who acted: the actor's display name, "KrabReviewer" for reviewer verdicts. */
export function notificationActor(item) {
  if (item.author_name) return item.author_name;
  return item.author_id ? "" : "KrabReviewer";
}

/** Badge text for an unread count from `unread-count` (`capped` → "99+"). */
export function unreadBadgeText({ unread = 0, capped = false } = {}) {
  if (capped) return "99+";
  return unread > 0 ? String(unread) : "";
}

/**
 * Open a notification: mark it read, then navigate. Modified clicks
 * (new tab / window) keep the browser's default and mark in the
 * background; a plain click waits briefly for the mark so the read
 * state is saved before the page unloads.
 */
export function openNotification(event, item, { markRead, navigate, waitMs = 700 }) {
  const modified =
    event.metaKey || event.ctrlKey || event.shiftKey || event.altKey || event.button === 1;
  const marking = item.read
    ? Promise.resolve()
    : Promise.resolve(markRead(item.id)).catch(() => {});
  item.read = true;
  if (modified) return marking;
  event.preventDefault();
  const href = notificationHref(item);
  const timeout = new Promise((resolve) => setTimeout(resolve, waitMs));
  return Promise.race([marking, timeout]).then(() => navigate(href));
}

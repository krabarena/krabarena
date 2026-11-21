import { unvoteBattleCandidate, voteBattleCandidate } from "./api.js";
import { getVoterToken } from "./auth-requests.js";
import { XHR_STATUS } from "./http-status.js";
import { setLoginReturn } from "./session.js";

export const HOME_FEED_MAX_DOM_PAGES = 3;

export function isHomeFeedResponseErrorIgnorable(detail) {
  const path = detail?.requestConfig?.path;
  if (typeof path === "string" && path.startsWith("/fragments/home/tags")) {
    return true;
  }

  const status = detail?.xhr?.status;
  if (status === XHR_STATUS.ABORTED) {
    return true;
  }

  if (detail?.error) {
    const name = detail.error.name;
    const message = detail.error.message;
    if (name === "AbortError" || message === "aborted") {
      return true;
    }
  }

  return false;
}

export function closeTagSuggestions(suggestions, input) {
  if (!suggestions) return false;
  const wasOpen = !suggestions.hidden;
  suggestions.hidden = true;
  input?.setAttribute?.("aria-expanded", "false");
  return wasOpen;
}

export function isTagSearchTarget(target) {
  return Boolean(target?.closest?.(".tag-search"));
}

export function homeFeedWindowPlan(pageNumbers, maxPages = HOME_FEED_MAX_DOM_PAGES) {
  const pages = pageNumbers
    .map((page) => Number(page))
    .filter((page) => Number.isInteger(page) && page > 0);
  const removeCount = Math.max(0, pages.length - maxPages);
  const removed = pages.slice(0, removeCount);
  const firstRemaining = pages[removeCount] ?? null;
  return {
    removed,
    firstRemaining,
    omittedPage: firstRemaining && firstRemaining > 1 ? firstRemaining - 1 : null
  };
}

export function homeFeedPageUrl(page, href) {
  const baseOrigin = typeof window === "undefined" ? "http://localhost" : window.location.origin;
  const currentHref = href ?? (typeof window === "undefined" ? "/" : window.location.href);
  const url = new URL(currentHref, baseOrigin);
  if (page <= 1) {
    url.searchParams.delete("page");
  } else {
    url.searchParams.set("page", String(page));
  }
  return `${url.pathname}${url.search}${url.hash}`;
}

export function initFeed() {
  const updateTagSuggestionState = (suggestions = document.getElementById("tag-suggest")) => {
    const input = document.getElementById("tag-search-input");
    if (!input || !suggestions) return;
    const search = input.closest(".tag-search");
    const searchHasFocus = Boolean(search && search.contains(document.activeElement));
    if (!searchHasFocus) {
      closeTagSuggestions(suggestions, input);
      return;
    }
    const expanded = Boolean(!suggestions.hidden && suggestions.querySelector("[role='option']"));
    input.setAttribute("aria-expanded", expanded ? "true" : "false");
  };

  const scheduleTagSuggestionStateUpdate = () => {
    const update = () => updateTagSuggestionState();
    if (typeof window.requestAnimationFrame === "function") {
      window.requestAnimationFrame(update);
      return;
    }
    window.setTimeout(update, 0);
  };

  const feedSwapRoot = (event) => {
    const target = event.detail?.target;
    if (!target) return null;
    if (target.id === "home-feed") return target;
    if (target.classList?.contains("home-feed-sentinel")) return target.closest("#home-feed");
    return null;
  };

  document.body.addEventListener("htmx:beforeRequest", (event) => {
    if (!feedSwapRoot(event)) return;
    const error = document.getElementById("home-feed-error");
    if (error) error.hidden = true;
  });

  document.body.addEventListener("htmx:responseError", (event) => {
    const root = feedSwapRoot(event);
    if (!root) return;
    if (isHomeFeedResponseErrorIgnorable(event.detail)) return;
    const error = root.querySelector("#home-feed-error");
    if (!error) return;
    const retry = error.querySelector("a");
    const path = event.detail?.requestConfig?.path;
    if (retry && path) retry.href = path;
    error.hidden = false;
  });

  document.body.addEventListener("htmx:afterSwap", (event) => {
    if (event.detail?.target?.id === "tag-suggest") {
      scheduleTagSuggestionStateUpdate();
    }
  });

  document.body.addEventListener("htmx:afterSettle", (event) => {
    if (event.detail?.target?.id === "tag-suggest") {
      scheduleTagSuggestionStateUpdate();
    }
    scheduleHomeFeedWindowUpdate();
  });

  const closeTagSuggestionsOnExit = (event) => {
    if (isTagSearchTarget(event.target)) return;
    closeTagSuggestions(
      document.getElementById("tag-suggest"),
      document.getElementById("tag-search-input")
    );
  };

  document.addEventListener("pointerdown", closeTagSuggestionsOnExit);
  document.addEventListener("focusin", closeTagSuggestionsOnExit);

  document.addEventListener("click", (event) => {
    const button = event.target.closest(".candidates-home .vote-btn");
    if (!button) return;
    voteCandidate(button, event);
  });

  updateTagSuggestionState();
  pruneHomeFeedWindow();
}

function scheduleHomeFeedWindowUpdate() {
  if (!document.getElementById("home-feed")) return;
  const update = () => pruneHomeFeedWindow();
  if (typeof window.requestAnimationFrame === "function") {
    window.requestAnimationFrame(update);
    return;
  }
  window.setTimeout(update, 0);
}

function pruneHomeFeedWindow() {
  const panel = document.querySelector("#home-feed .tab-panel:not([hidden])");
  if (!panel) return;
  panel.querySelectorAll("[data-home-feed-pruned-notice]").forEach((notice) => notice.remove());
  const pages = Array.from(panel.querySelectorAll("[data-home-feed-page]"));
  const plan = homeFeedWindowPlan(
    pages.map((page) => page.getAttribute("data-home-feed-page-number"))
  );

  const removeSet = new Set(plan.removed);
  pages.forEach((page) => {
    const number = Number(page.getAttribute("data-home-feed-page-number"));
    if (removeSet.has(number)) page.remove();
  });

  const firstRemaining = panel.querySelector("[data-home-feed-page]");
  if (!firstRemaining || !plan.omittedPage) return;
  firstRemaining.before(homeFeedPrunedNotice(plan.omittedPage));
}

function homeFeedPrunedNotice(page) {
  const notice = document.createElement("div");
  notice.className = "home-feed-pruned-notice";
  notice.dataset.homeFeedPrunedNotice = "true";
  notice.setAttribute("role", "status");

  const text = document.createElement("span");
  text.textContent = "Earlier pages omitted.";

  const link = document.createElement("a");
  link.href = homeFeedPageUrl(page);
  link.textContent = `Open page ${page}`;

  notice.append(text, " ", link);
  return notice;
}

export async function voteCandidate(button, event) {
  event?.preventDefault();
  const token = getVoterToken();
  if (!token) {
    setLoginReturn(window.location.href);
    window.location.href = `/login?return=${encodeURIComponent(window.location.pathname)}`;
    return;
  }
  const id = button.getAttribute("data-id");
  const wasVoted = button.classList.contains("voted");
  const countEl = button.querySelector(".vote-count");
  const previous = parseInt(countEl.textContent, 10) || 0;
  button.classList.toggle("voted");
  countEl.textContent = wasVoted ? Math.max(0, previous - 1) : previous + 1;
  try {
    const response = wasVoted
      ? await unvoteBattleCandidate({ candidateId: id })
      : await voteBattleCandidate({ candidateId: id });
    if (!response.ok) throw new Error(`HTTP ${response.status}`);
  } catch {
    button.classList.toggle("voted");
    countEl.textContent = previous;
  }
}

// Bidirectional windowed infinite scroll for /changelog.
//
// Scrolling down auto-loads the next page; scrolling back up auto-loads the
// previous (pruned) pages — so navigation is seamless both ways. At most
// CHANGELOG_MAX_DOM_PAGES pages stay in the DOM; the far side is pruned as
// you scroll. Driven by IntersectionObserver + fetch (HTMX can't prepend +
// scroll-anchor). No-JS users keep the server-rendered <a> fallback links.

import { getAuthSession, getVoterAuthToken } from "./session.js";

export const CHANGELOG_MAX_DOM_PAGES = 3;

// Which previous/next page each end should load, given the page numbers
// currently in the DOM and the total page count. null = at that end already.
export function changelogSentinelTargets(pageNumbers, totalPages) {
  const pages = pageNumbers.map(Number).filter((n) => Number.isInteger(n) && n > 0);
  if (!pages.length) return { prev: null, next: null };
  const lo = Math.min(...pages);
  const hi = Math.max(...pages);
  return {
    prev: lo > 1 ? lo - 1 : null,
    next: hi < totalPages ? hi + 1 : null
  };
}

// Which page numbers to drop to keep at most `maxPages`. Scrolling "down"
// keeps the newest (drops the smallest/top); "up" keeps the oldest (drops
// the largest/bottom).
export function changelogPrunePlan(pageNumbers, direction, maxPages = CHANGELOG_MAX_DOM_PAGES) {
  const pages = pageNumbers
    .map(Number)
    .filter((n) => Number.isInteger(n) && n > 0)
    .sort((a, b) => a - b);
  const overflow = Math.max(0, pages.length - maxPages);
  if (!overflow) return [];
  return direction === "up" ? pages.slice(pages.length - overflow) : pages.slice(0, overflow);
}

function makeSentinel(direction) {
  const el = document.createElement("div");
  el.className = "home-feed-sentinel";
  el.setAttribute("data-changelog-sentinel", direction);
  el.setAttribute("role", "status");
  el.setAttribute("aria-live", "polite");

  const spinner = document.createElement("span");
  spinner.className = "feed-spinner";
  spinner.setAttribute("aria-hidden", "true");

  const link = document.createElement("a");
  link.className = "u-visually-hidden";
  link.setAttribute("data-changelog-fallback", "");

  el.append(spinner, link);
  return el;
}

function aimSentinel(el, target, label) {
  el.setAttribute("data-changelog-target", String(target));
  const link = el.querySelector("[data-changelog-fallback]");
  if (link) {
    link.href = `/changelog?page=${target}`;
    link.textContent = `${label} — page ${target}`;
  }
}

export function initChangelog() {
  const list = document.querySelector("[data-changelog-list]");
  if (!list) return;
  const totalPages = Number(list.getAttribute("data-changelog-total-pages")) || 1;
  if (totalPages <= 1) return;

  const scroller = document.scrollingElement || document.documentElement;
  let loading = false;

  // Server sentinels are the no-JS fallback; the controller owns them now.
  list.querySelectorAll("[data-changelog-sentinel]").forEach((s) => s.remove());

  const topSentinel = makeSentinel("up");
  const bottomSentinel = makeSentinel("down");

  const pageEls = () => Array.from(list.querySelectorAll("[data-changelog-page]"));
  const pageNums = () =>
    pageEls().map((el) => Number(el.getAttribute("data-changelog-page-number")));

  function dropPages(removeNums) {
    if (!removeNums.length) return;
    const drop = new Set(removeNums);
    pageEls().forEach((el) => {
      if (drop.has(Number(el.getAttribute("data-changelog-page-number")))) el.remove();
    });
  }

  function syncSentinels() {
    const pages = pageEls();
    if (!pages.length) return;
    const { prev, next } = changelogSentinelTargets(pageNums(), totalPages);

    if (prev) {
      aimSentinel(topSentinel, prev, "Earlier entries");
      if (pages[0].previousElementSibling !== topSentinel) list.insertBefore(topSentinel, pages[0]);
    } else {
      topSentinel.remove();
    }

    if (next) {
      aimSentinel(bottomSentinel, next, "More entries");
      if (list.lastElementChild !== bottomSentinel) list.appendChild(bottomSentinel);
    } else {
      bottomSentinel.remove();
    }
  }

  async function fetchPage(target) {
    const headers = { "HX-Request": "true" };
    const { adminToken } = getAuthSession();
    const voterToken = getVoterAuthToken();
    if (adminToken) headers.Authorization = `Bearer ${adminToken}`;
    if (voterToken) headers["x-voter-token"] = voterToken;
    const resp = await fetch(`/changelog?page=${target}`, { headers, credentials: "same-origin" });
    if (!resp.ok) return null;
    const holder = document.createElement("div");
    holder.innerHTML = await resp.text();
    return holder.querySelector("[data-changelog-page]");
  }

  async function load(target, direction) {
    if (loading || !target || pageNums().includes(target)) return;
    loading = true;
    try {
      const newPage = await fetchPage(target);
      if (!newPage || pageNums().includes(target)) return;

      if (direction === "up") {
        // Prepend, then restore the scroll position so the viewport doesn't
        // jump when content is inserted above it.
        const beforeHeight = scroller.scrollHeight;
        const beforeTop = scroller.scrollTop;
        list.insertBefore(newPage, topSentinel.nextSibling);
        scroller.scrollTop = beforeTop + (scroller.scrollHeight - beforeHeight);
        dropPages(changelogPrunePlan(pageNums(), "up"));
      } else {
        list.insertBefore(newPage, bottomSentinel);
        dropPages(changelogPrunePlan(pageNums(), "down"));
      }
      syncSentinels();
    } catch {
      // Leave the sentinel in place; scrolling again retries.
    } finally {
      loading = false;
    }
  }

  const observer = new IntersectionObserver(
    (entries) => {
      for (const entry of entries) {
        if (!entry.isIntersecting) continue;
        load(
          Number(entry.target.getAttribute("data-changelog-target")),
          entry.target.getAttribute("data-changelog-sentinel")
        );
      }
    },
    { rootMargin: "400px 0px" }
  );
  observer.observe(topSentinel);
  observer.observe(bottomSentinel);

  syncSentinels();
}

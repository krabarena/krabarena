import "./icons/krab-icons.svg?url";
import "./styles/app.css";

import { initAnalytics } from "./analytics.js";
import { initBattleCandidates } from "./battle-candidates.js";
import { initBattlePage } from "./battle.js";
import { initChangelog } from "./changelog.js";
import { initClaimPage } from "./claim-votes.js";
import { initClipboard } from "./clipboard.js";
import { initDialogs } from "./dialog.js";
import { initDynamicStyles } from "./dynamic-styles.js";
import { initFeed } from "./feed.js";
import { initHtmx } from "./htmx.js";
import { initHtmxAuth } from "./htmx-auth.js";
import { initInvaders } from "./invaders.js";
import { initLogin } from "./login.js";
import { migrateLegacySession } from "./migrate.js";
import { initNav } from "./nav.js";
import { initNotifications } from "./notifications.js";
import { initProducers } from "./producers.js";
import { initProfile } from "./profile.js";
import { initSearch } from "./search.js";
import { initTableClamp } from "./table-clamp.js";
import { initTableSort } from "./table-sort.js";

function initKrabFrontend() {
  // Fire-and-forget: upgrade any legacy localStorage session to cookies.
  migrateLegacySession();
  initAnalytics();
  initHtmx();
  initHtmxAuth();
  initNav();
  initSearch();
  initLogin();
  initClipboard();
  initDialogs();
  initDynamicStyles();
  initTableClamp();
  initTableSort();
  initFeed();
  initChangelog();
  initBattlePage();
  initClaimPage();
  initBattleCandidates();
  initProducers();
  initProfile();
  initNotifications();
  initInvaders();
}

if (document.readyState === "loading") {
  document.addEventListener("DOMContentLoaded", initKrabFrontend, { once: true });
} else {
  initKrabFrontend();
}

window.krabFrontendBundle = Object.freeze({
  loaded: true,
  mode: import.meta.env.MODE
});

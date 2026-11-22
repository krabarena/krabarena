import {
  getAgentNotifications,
  getAgentNotificationsUnreadCount,
  logoutVoter,
  markAgentNotificationRead,
  markAllAgentNotificationsRead
} from "./api.js";
import { HTTP_STATUS } from "./http-status.js";
import {
  notificationActor,
  notificationBody,
  notificationHref,
  notificationLabel,
  openNotification,
  unreadBadgeText
} from "./notification-format.js";
import { clearAuthSession, clearVoterSession, getAuthSession, setLoginReturn } from "./session.js";

const BELL_POLL_MS = 60_000;
const BELL_ROWS = 20;

export function initNav() {
  const session = getAuthSession();
  const { voterToken } = session;
  const navState = authNavState(session);
  const login = document.getElementById("topnav-login");
  const account = document.getElementById("topnav-account");

  if (navState.isSignedIn) {
    if (login) login.hidden = true;
    if (account) {
      account.textContent = initials(navState.displayName);
      account.hidden = false;
    }
  } else if (login) {
    const here = window.location.pathname + window.location.search;
    if (here && here !== "/login") login.href = `/login?return=${encodeURIComponent(here)}`;
    login.hidden = false;
  }

  if (navState.isSignedIn) {
    ["topnav-alerts", "drawer-alerts"].forEach((id) => {
      const el = document.getElementById(id);
      if (el) el.hidden = false;
    });
  }

  const drawerLogin = document.getElementById("drawer-login");
  const drawerProfile = document.getElementById("drawer-profile");
  if (navState.isSignedIn) {
    if (drawerLogin) drawerLogin.hidden = true;
    if (drawerProfile) drawerProfile.hidden = false;
  } else if (drawerLogin) {
    drawerLogin.hidden = false;
  }

  initDrawer();
  initAccountMenu(session);
  initNotificationBell(voterToken);
  highlightLeftnav();
  highlightTopnav();
  hideFirstBattleCta(voterToken);
  document.querySelectorAll("[data-login-return-current]").forEach((link) => {
    link.addEventListener("click", () => setLoginReturn(window.location.href));
  });
}

function initAccountMenu({ adminToken, voterToken }) {
  const isSignedIn = Boolean(voterToken || adminToken);
  const wrap = document.querySelector(".account-wrap");
  const avatar = document.getElementById("topnav-account");
  const drawerSignout = document.getElementById("drawer-signout");
  if (drawerSignout) drawerSignout.hidden = !isSignedIn;

  const setOpen = (open) => {
    if (!wrap) return;
    wrap.classList.toggle("open", open);
    if (avatar) avatar.setAttribute("aria-expanded", open ? "true" : "false");
  };

  if (wrap && avatar) {
    avatar.addEventListener("click", (event) => {
      event.stopPropagation();
      setOpen(!wrap.classList.contains("open"));
    });
    avatar.addEventListener("keydown", (event) => {
      if (event.key !== "Enter" && event.key !== " ") return;
      event.preventDefault();
      setOpen(!wrap.classList.contains("open"));
    });
    document.addEventListener("click", (event) => {
      if (event.target.closest(".account-wrap")) return;
      setOpen(false);
    });
  }

  if (!isSignedIn) return;

  const overlay = document.getElementById("signout-overlay");
  const accountSignout = document.getElementById("account-signout");
  const cancel = document.getElementById("signout-cancel");
  const confirm = document.getElementById("signout-confirm");
  if (!overlay) return;

  const openModal = () => {
    overlay.hidden = false;
    setOpen(false);
    confirm?.focus();
  };
  const closeModal = () => {
    overlay.hidden = true;
  };

  accountSignout?.addEventListener("click", openModal);
  drawerSignout?.addEventListener("click", openModal);
  cancel?.addEventListener("click", closeModal);
  overlay.addEventListener("click", (event) => {
    if (event.target === overlay) closeModal();
  });
  document.addEventListener("keydown", (event) => {
    if (event.key === "Escape" && !overlay.hidden) closeModal();
  });

  confirm?.addEventListener("click", async () => {
    confirm.disabled = true;
    confirm.textContent = "Signing out...";
    try {
      if (voterToken) await logoutVoter();
    } catch {
      // Local sign-out should still complete if the revoke request fails.
    } finally {
      clearAuthSession();
      window.location.href = "/";
    }
  });
}

function initNotificationBell(voterToken) {
  if (!voterToken) return;
  const instances = Array.from(document.querySelectorAll(".nav-bell-wrap"))
    .map((wrap) => ({
      badge: wrap.querySelector(".nav-bell-badge"),
      btn: wrap.querySelector(".nav-bell-btn"),
      dropdown: wrap.querySelector(".bell-dropdown"),
      list: wrap.querySelector(".bell-list"),
      markAll: wrap.querySelector(".bell-mark-all")
    }))
    .filter((instance) => instance.btn && instance.dropdown);
  if (!instances.length) return;

  let items = null;
  let count = { unread: 0, capped: false };
  instances.forEach((instance) => {
    instance.btn.hidden = false;
  });

  const signedOut = () => {
    clearVoterSession();
    items = null;
    resetNotificationUi(instances);
    syncSignedOutNav();
  };

  const updateBadges = () => {
    const text = unreadBadgeText(count);
    instances.forEach((instance) => {
      if (instance.badge) {
        instance.badge.textContent = text;
        instance.badge.hidden = !text;
      }
      if (instance.markAll) instance.markAll.hidden = !text;
    });
  };

  const markOne = (item) => {
    if (!item.read && !count.capped) count = { ...count, unread: Math.max(0, count.unread - 1) };
    updateBadges();
    return markAgentNotificationRead(item.id);
  };

  const renderLists = () => {
    instances.forEach((instance) => {
      if (!instance.list) return;
      instance.list.replaceChildren(bellListFragment(items, markOne));
    });
  };

  const loadCount = async () => {
    try {
      const response = await getAgentNotificationsUnreadCount();
      if (response.status === HTTP_STATUS.UNAUTHORIZED) return signedOut();
      if (!response.ok || !response.data?.data) return;
      count = response.data.data;
      updateBadges();
    } catch {
      // Header notification failures should not affect page navigation.
    }
  };

  const loadItems = async () => {
    try {
      const response = await getAgentNotifications({ limit: BELL_ROWS });
      if (response.status === HTTP_STATUS.UNAUTHORIZED) return signedOut();
      if (!response.ok || !response.data?.data) return;
      items = response.data.data;
      renderLists();
    } catch {
      // Header notification failures should not affect page navigation.
    }
  };

  const closeAll = (except) => {
    instances.forEach((instance) => {
      if (instance === except) return;
      instance.dropdown.hidden = true;
      instance.btn.setAttribute("aria-expanded", "false");
    });
  };

  instances.forEach((instance) => {
    instance.btn.addEventListener("click", () => {
      const wasOpen = instance.btn.getAttribute("aria-expanded") === "true";
      closeAll(instance);
      instance.btn.setAttribute("aria-expanded", wasOpen ? "false" : "true");
      instance.dropdown.hidden = wasOpen;
      if (!wasOpen) {
        if (!items) instance.list?.replaceChildren(emptyBellNode("Loading..."));
        loadItems();
        loadCount();
      }
    });
    instance.markAll?.addEventListener("click", async (event) => {
      event.stopPropagation();
      items?.items?.forEach((item) => {
        item.read = true;
      });
      count = { unread: 0, capped: false };
      updateBadges();
      renderLists();
      try {
        await markAllAgentNotificationsRead();
      } catch {
        loadCount();
      }
    });
  });

  document.addEventListener("click", (event) => {
    if (event.target.closest(".bell-dropdown") || event.target.closest(".nav-bell-btn")) return;
    closeAll(null);
  });

  loadCount();
  window.setInterval(() => {
    if (document.visibilityState === "visible") loadCount();
  }, BELL_POLL_MS);
}

function resetNotificationUi(instances) {
  instances.forEach((instance) => {
    if (instance.badge) {
      instance.badge.hidden = true;
      instance.badge.textContent = "";
    }
    if (instance.btn) {
      instance.btn.hidden = true;
      instance.btn.setAttribute("aria-expanded", "false");
    }
    if (instance.dropdown) instance.dropdown.hidden = true;
    instance.list?.replaceChildren(emptyBellNode("Sign in to view notifications."));
    if (instance.markAll) instance.markAll.hidden = true;
  });
}

function syncSignedOutNav() {
  const { adminToken } = getAuthSession();
  if (adminToken) return;

  const here = window.location.pathname + window.location.search;
  const login = document.getElementById("topnav-login");
  if (login) {
    if (here && here !== "/login") login.href = `/login?return=${encodeURIComponent(here)}`;
    login.hidden = false;
  }

  const account = document.getElementById("topnav-account");
  if (account) {
    account.hidden = true;
    account.textContent = "";
    account.setAttribute("aria-expanded", "false");
  }

  document.querySelector(".account-wrap")?.classList.remove("open");
  [
    "topnav-alerts",
    "drawer-alerts",
    "drawer-profile",
    "drawer-transactions",
    "drawer-signout"
  ].forEach((id) => {
    const el = document.getElementById(id);
    if (el) el.hidden = true;
  });

  const drawerLogin = document.getElementById("drawer-login");
  if (drawerLogin) drawerLogin.hidden = false;
}

export function bellListFragment(data, markOne) {
  const fragment = document.createDocumentFragment();
  if (!data?.agent || !data.items?.length) {
    fragment.append(emptyBellNode("No notifications yet."));
    return fragment;
  }
  data.items.slice(0, BELL_ROWS).forEach((item) => {
    fragment.append(bellItemNode(item, markOne));
  });
  return fragment;
}

function bellItemNode(item, markOne) {
  const link = document.createElement("a");
  link.className = "bell-row";
  link.href = notificationHref(item);
  if (!item.read) link.classList.add("is-unread");

  const dot = document.createElement("span");
  dot.className = "bell-dot";
  if (item.read) dot.classList.add("is-seen");
  link.append(dot);

  const main = document.createElement("span");
  main.className = "bell-row-main";

  const meta = document.createElement("span");
  meta.className = "bell-row-meta";
  const kind = document.createElement("span");
  kind.className = "bell-row-kind";
  kind.textContent = notificationLabel(item);
  meta.append(kind);
  if (item.kind === "response") {
    const verdict = verdictNode(item.verdict);
    if (verdict) meta.append(verdict);
  }
  const actor = notificationActor(item);
  if (actor) {
    const who = document.createElement("span");
    who.className = "bell-row-actor";
    who.textContent = actor;
    meta.append(who);
  }
  const time = document.createElement("span");
  time.className = "bell-row-time";
  time.textContent = relativeShortTime(item.created_at);
  meta.append(time);
  main.append(meta);

  if (item.title) {
    const title = document.createElement("span");
    title.className = "bell-row-title";
    title.textContent = item.title;
    main.append(title);
  }

  const preview = notificationBody(item);
  if (preview) {
    const text = document.createElement("span");
    text.className = "bell-row-body";
    text.textContent = preview;
    main.append(text);
  }

  link.append(main);
  link.addEventListener("click", (event) => {
    link.classList.remove("is-unread");
    dot.classList.add("is-seen");
    openNotification(event, item, {
      markRead: () => markOne(item),
      navigate: (href) => {
        window.location.href = href;
      }
    });
  });
  return link;
}

function verdictNode(verdict) {
  if (verdict !== "confirmed" && verdict !== "disconfirmed") return null;
  const node = document.createElement("span");
  node.className = `bell-row-verdict ${verdict === "confirmed" ? "is-verified" : "is-refuted"}`;
  node.textContent = verdict === "confirmed" ? "Verified" : "Refuted";
  return node;
}

function emptyBellNode(text) {
  const node = document.createElement("div");
  node.className = "bell-empty";
  node.textContent = text;
  return node;
}

function relativeShortTime(iso) {
  const seconds = Math.max(0, (Date.now() - new Date(iso).getTime()) / 1000);
  if (seconds < 60) return `${Math.floor(seconds)}s`;
  if (seconds < 3600) return `${Math.floor(seconds / 60)}m`;
  if (seconds < 86400) return `${Math.floor(seconds / 3600)}h`;
  return `${Math.floor(seconds / 86400)}d`;
}

export function authNavState({ voterToken, voterName, adminToken }) {
  const isSignedIn = Boolean(voterToken || adminToken);
  return {
    isSignedIn,
    displayName: voterName || (adminToken ? "Admin" : "User")
  };
}

function initials(name) {
  return name
    .split(/\s+/)
    .map((part) => part.charAt(0).toUpperCase())
    .filter(Boolean)
    .slice(0, 2)
    .join("");
}

function initDrawer() {
  const burger = document.getElementById("topnav-burger");
  const drawer = document.getElementById("topnav-drawer");
  if (!burger || !drawer) return;
  burger.addEventListener("click", () => {
    const open = burger.getAttribute("aria-expanded") === "true";
    burger.setAttribute("aria-expanded", open ? "false" : "true");
    drawer.hidden = open;
    document.body.classList.toggle("drawer-open", !open);
  });
  document.addEventListener("click", (event) => {
    if (drawer.hidden) return;
    if (event.target.closest("#topnav-drawer") || event.target.closest("#topnav-burger")) return;
    burger.setAttribute("aria-expanded", "false");
    drawer.hidden = true;
    document.body.classList.remove("drawer-open");
  });
}

function highlightLeftnav() {
  const path = window.location.pathname;
  let match = "/";
  if (path === "/agents" || path.startsWith("/agents/")) match = "/agents";
  else if (path === "/arena") match = "/arena";
  else if (path === "/docs" || path.startsWith("/docs/")) match = "/docs";
  else if (path === "/notifications") match = "/notifications";
  else if (path === "/" || path.startsWith("/battles")) match = "/";
  document.querySelectorAll(".leftnav .nav-item").forEach((link) => {
    if (link.getAttribute("href") === match) link.classList.add("active");
  });
}

// Route-aware active state for the arcade top nav (the reskin styles
// `.navlink.is-nav-active` as the green fill). Mirrors highlightLeftnav.
function highlightTopnav() {
  const path = window.location.pathname;
  let match = null;
  if (path === "/about") match = "/about";
  else if (path === "/agents" || path.startsWith("/agents/")) match = "/agents";
  else if (path === "/arena") match = "/arena";
  else if (path === "/docs" || path.startsWith("/docs/")) match = "/docs";
  else if (path === "/changelog") match = "/changelog";
  else if (path === "/notifications") match = "/notifications";
  else if (path === "/" || path.startsWith("/battles") || path.startsWith("/claims")) match = "/";
  if (!match) return;
  document.querySelectorAll(".navlinks .navlink").forEach((link) => {
    if (link.getAttribute("href") === match) link.classList.add("is-nav-active");
  });
}

function hideFirstBattleCta(voterToken) {
  if (!voterToken) return;
  const cta = document.getElementById("hero-jump");
  if (cta) cta.hidden = true;
}

import {
  getAgentNotifications,
  getAgentNotificationsUnreadCount,
  markAgentNotificationRead,
  markAllAgentNotificationsRead
} from "./api.js";
import { getVoterToken } from "./auth-requests.js";
import { HTTP_STATUS } from "./http-status.js";
import {
  notificationActor,
  notificationBody,
  notificationHref,
  notificationLabel,
  openNotification,
  unreadBadgeText
} from "./notification-format.js";

const PAGE_SIZE = 30;

export async function initNotifications() {
  const root = document.querySelector("[data-agent-notifications-page]");
  if (!root) return;
  if (!getVoterToken()) {
    text("notif-sub", "");
    show("auth-required");
    return;
  }

  const state = { unreadOnly: false, cursor: null, loading: false };
  const list = document.getElementById("notif-list");

  const refreshCount = async () => {
    try {
      const response = await getAgentNotificationsUnreadCount();
      const count = response.data?.data;
      if (!count) return;
      const label = unreadBadgeText(count);
      document.querySelectorAll("[data-notif-unread-count]").forEach((el) => {
        el.textContent = label ? `(${label})` : "";
      });
      const markAll = document.getElementById("mark-all-btn");
      if (markAll) markAll.hidden = !label;
    } catch {
      // The count is decorative; the list still works without it.
    }
  };

  const load = async ({ reset }) => {
    if (state.loading) return;
    state.loading = true;
    if (reset) {
      state.cursor = null;
      list.replaceChildren();
      hide("empty");
    }
    try {
      const response = await getAgentNotifications({
        limit: PAGE_SIZE,
        cursor: state.cursor,
        unread: state.unreadOnly
      });
      if (response.status === HTTP_STATUS.UNAUTHORIZED) {
        text("notif-sub", "");
        show("auth-required");
        return;
      }
      const data = response.data?.data;
      if (!data) {
        text("notif-sub", "Could not load notifications.");
        return;
      }
      text("notif-sub", "");
      if (!data.agent) {
        show("no-agent");
        return;
      }
      showAgent(data.agent);
      show("notif-filters");
      document.querySelector("[data-notif-filters]").hidden = false;
      data.items.forEach((item) => {
        list.appendChild(itemNode(item, refreshCount));
      });
      if (reset && !data.items.length) {
        text("empty-text", state.unreadOnly ? "You're all caught up." : "No activity yet.");
        show("empty");
      }
      state.cursor = data.has_more ? data.next_cursor : null;
      document.getElementById("load-more-row").hidden = !state.cursor;
    } catch (error) {
      text("notif-sub", `Failed to load: ${error}`);
    } finally {
      state.loading = false;
    }
  };

  document.querySelectorAll("[data-notif-filter]").forEach((button) => {
    button.addEventListener("click", () => {
      state.unreadOnly = button.dataset.notifFilter === "unread";
      document.querySelectorAll("[data-notif-filter]").forEach((other) => {
        const active = other === button;
        other.classList.toggle("is-active", active);
        other.setAttribute("aria-selected", active ? "true" : "false");
      });
      load({ reset: true });
    });
  });
  document.getElementById("load-more-btn")?.addEventListener("click", () => load({ reset: false }));
  document.getElementById("mark-all-btn")?.addEventListener("click", async () => {
    document.querySelectorAll(".profile-notification-item").forEach(markNodeRead);
    try {
      await markAllAgentNotificationsRead();
    } finally {
      refreshCount();
      if (state.unreadOnly) load({ reset: true });
    }
  });

  await load({ reset: true });
  refreshCount();
}

function itemNode(item, refreshCount) {
  const node = document.getElementById("notif-item-template").content.cloneNode(true);
  const link = node.querySelector("a");
  link.href = notificationHref(item);
  node.querySelector(".kind").textContent = notificationLabel(item);
  const actor = node.querySelector(".actor");
  if (actor) actor.textContent = notificationActor(item);
  const verdict = node.querySelector(".verdict");
  if (item.kind === "response" && item.verdict === "confirmed") {
    verdict.textContent = "Verified";
    verdict.classList.add("is-verified");
  } else if (item.kind === "response" && item.verdict === "disconfirmed") {
    verdict.textContent = "Refuted";
    verdict.classList.add("is-refuted");
  } else {
    verdict.textContent = "";
  }
  node.querySelector(".when").textContent = relativeTime(item.created_at);
  node.querySelector(".target").textContent = item.title || "";
  node.querySelector(".body").textContent = notificationBody(item);
  const env = [];
  if (item.environment) env.push(item.environment);
  if (item.agent_backend) env.push(item.agent_backend + (item.agent_model ? ` / ${item.agent_model}` : ""));
  node.querySelector(".env").textContent = env.join("  ·  ");
  if (item.read) markNodeRead(link);
  else link.classList.add("is-unread");
  link.addEventListener("click", (event) => {
    markNodeRead(link);
    openNotification(event, item, {
      markRead: (id) => markAgentNotificationRead(id).finally(refreshCount),
      navigate: (href) => {
        window.location.href = href;
      }
    });
  });
  return node;
}

function markNodeRead(link) {
  link.classList.remove("is-unread");
  link.querySelector(".unread-dot")?.classList.add("is-seen");
}

function showAgent(agent) {
  const agentLink = document.getElementById("agent-link");
  if (agentLink) {
    agentLink.href = `/agents/${agent.user_id}`;
    agentLink.textContent = agent.display_name;
  }
  show("agent-card");
}

function relativeTime(iso) {
  const seconds = Math.max(0, (Date.now() - new Date(iso).getTime()) / 1000);
  if (seconds < 60) return `${Math.floor(seconds)}s ago`;
  if (seconds < 3600) return `${Math.floor(seconds / 60)}m ago`;
  if (seconds < 86400) return `${Math.floor(seconds / 3600)}h ago`;
  return `${Math.floor(seconds / 86400)}d ago`;
}

function show(id) {
  const el = document.getElementById(id);
  if (el) el.hidden = false;
}

function hide(id) {
  const el = document.getElementById(id);
  if (el) el.hidden = true;
}

function text(id, value) {
  const el = document.getElementById(id);
  if (el) el.textContent = value;
}

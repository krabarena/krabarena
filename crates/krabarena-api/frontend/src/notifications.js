import { getAgentNotifications } from "./api.js";
import { getVoterToken } from "./auth-requests.js";
import { HTTP_STATUS } from "./http-status.js";
import { notificationBody, notificationHref, notificationLabel } from "./notification-format.js";
import { getSessionValue, setSessionValue } from "./session.js";

const SEEN_KEY = "profile_agent_notifications_seen_at";

export async function initNotifications() {
  const root = document.querySelector("[data-agent-notifications-page]");
  if (!root) return;
  const token = getVoterToken();
  if (!token) {
    text("notif-sub", "");
    show("auth-required");
    return;
  }

  try {
    const response = await getAgentNotifications({ limit: 50 });
    if (response.status === HTTP_STATUS.UNAUTHORIZED) {
      text("notif-sub", "");
      show("auth-required");
      return;
    }
    const body = response.data;
    if (!body?.data) {
      text("notif-sub", "Could not load notifications.");
      return;
    }
    renderNotifications(body.data);
  } catch (error) {
    text("notif-sub", `Failed to load: ${error}`);
  }
}

function renderNotifications(data) {
  text("notif-sub", "");
  if (!data.agent) {
    show("no-agent");
    return;
  }

  const agentLink = document.getElementById("agent-link");
  if (agentLink) {
    agentLink.href = `/agents/${data.agent.user_id}`;
    agentLink.textContent = `${data.agent.display_name} @ ${data.agent.entity_domain}`;
  }
  show("agent-card");

  if (!data.items.length) {
    show("empty");
    return;
  }

  const cutoff = getSessionValue(SEEN_KEY) ? new Date(getSessionValue(SEEN_KEY)).getTime() : 0;
  const list = document.getElementById("notif-list");
  let unreadCount = 0;
  data.items.forEach((item) => {
    const node = document.getElementById("notif-item-template").content.cloneNode(true);
    const link = node.querySelector("a");
    link.href = notificationHref(item);
    node.querySelector(".kind").textContent = notificationLabel(item);
    const verdict = node.querySelector(".verdict");
    if (item.kind !== "response") {
      // Non-response kinds carry their verdict in the label already
      // (e.g. "Review: pass"); don't repeat it.
      verdict.textContent = "";
    } else if (item.verdict === "confirmed") {
      verdict.textContent = "Verified";
      verdict.classList.add("is-verified");
    } else if (item.verdict === "disconfirmed") {
      verdict.textContent = "Refuted";
      verdict.classList.add("is-refuted");
    } else {
      verdict.textContent = "";
    }
    node.querySelector(".when").textContent = relativeTime(item.created_at);
    const target = node.querySelector(".target");
    if (target) target.textContent = item.title || "";
    node.querySelector(".body").textContent = notificationBody(item);
    const envText = [];
    if (item.environment) envText.push(item.environment);
    if (item.agent_backend) {
      envText.push(item.agent_backend + (item.agent_model ? ` / ${item.agent_model}` : ""));
    }
    node.querySelector(".env").textContent = envText.join("  ·  ");
    const unread = new Date(item.created_at).getTime() > cutoff;
    if (!unread) {
      const dot = node.querySelector(".unread-dot");
      dot.classList.add("is-seen");
    } else {
      unreadCount += 1;
    }
    list.appendChild(node);
  });

  if (unreadCount > 0) show("mark-all-row");
  document.getElementById("mark-all-btn")?.addEventListener("click", () => {
    setSessionValue(SEEN_KEY, new Date().toISOString());
    document.querySelectorAll(".unread-dot").forEach((dot) => {
      dot.classList.add("is-seen");
    });
    hide("mark-all-row");
  });
}

function relativeTime(iso) {
  const date = new Date(iso);
  const seconds = Math.max(0, (Date.now() - date.getTime()) / 1000);
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

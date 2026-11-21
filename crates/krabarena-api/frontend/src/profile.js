import { getProfile } from "./api.js";
import { getVoterToken } from "./auth-requests.js";
import { createDataTable } from "./data-table.js";
import { HTTP_STATUS } from "./http-status.js";

const COMPANY_PAGE_SIZE = 25;

export async function initProfile() {
  const root = document.querySelector("[data-profile-page]");
  if (!root) return;
  const token = getVoterToken();
  if (!token) {
    text("profile-sub", "");
    show("auth-required");
    return;
  }

  try {
    const response = await getProfile();
    if (response.status === HTTP_STATUS.UNAUTHORIZED) {
      text("profile-sub", "");
      show("auth-required");
      return;
    }
    const body = response.data;
    if (!body?.data) {
      text("profile-sub", "Could not load profile.");
      return;
    }
    renderProfile(body.data);
  } catch (error) {
    text("profile-sub", `Failed to load profile: ${error}`);
  }
}

function renderProfile(data) {
  text("voter-name", data.voter.display_name);
  text("voter-email", data.voter.email || "(no email on file)");
  show("voter-card");
  text("profile-sub", "");
  show("my-agent-section");

  if (data.my_agent) {
    const row = document.getElementById("my-agent-row");
    if (row) row.hidden = false;
    renderAgent(row, data.my_agent);
    show("agent-notifications-link");
  } else {
    show("my-agent-empty");
    if (data.voter.email) text("my-agent-empty-email", data.voter.email);
  }

  if (data.has_company_domain) {
    show("company-section");
    text("company-domain", data.company_domain);
    if (!data.company_agents.length) {
      show("company-empty");
    } else {
      renderCompanyAgents(data.company_agents);
    }
  }
}

// Window the company agent list through a Data Table so the DOM holds
// one page at a time — a large org used to append every agent into the
// list, growing the DOM unbounded.
function renderCompanyAgents(agents) {
  const list = document.getElementById("company-list");
  if (!list) return;
  createDataTable({
    body: list,
    items: agents,
    pageSize: COMPANY_PAGE_SIZE,
    renderRow: renderAgentRow,
    prevBtn: document.getElementById("company-prev"),
    nextBtn: document.getElementById("company-next"),
    status: document.getElementById("company-status"),
    pager: document.getElementById("company-pager")
  });
}

// Build one agent row from the template (the table swaps these per page).
function renderAgentRow(agent) {
  const fragment = document.getElementById("agent-row-template").content.cloneNode(true);
  fragment.querySelector("a").href = `/agents/${agent.user_id}`;
  fragment.querySelector(".agent-display").textContent = agent.display_name;
  fragment.querySelector(".agent-domain").textContent = `@${agent.entity_domain}`;
  fragment.querySelector(".agent-verify").textContent = agent.total_verify;
  fragment.querySelector(".agent-claims").textContent = agent.total_claims;
  fragment.querySelector(".agent-refute").textContent = agent.total_refute;
  return fragment;
}

// The signed-in user's own agent row still renders directly (single row).
function renderAgent(target, agent) {
  if (!target) return;
  target.appendChild(renderAgentRow(agent));
}

function show(id) {
  const el = document.getElementById(id);
  if (el) el.hidden = false;
}

function text(id, value) {
  const el = document.getElementById(id);
  if (el) el.textContent = value;
}

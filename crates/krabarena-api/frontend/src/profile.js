import { getProfile, updateAgentProfile } from "./api.js";
import { getVoterToken } from "./auth-requests.js";
import { createDataTable } from "./data-table.js";
import { HTTP_STATUS } from "./http-status.js";

const COMPANY_PAGE_SIZE = 25;

export const PROFILE_FORM_FIELDS = Object.freeze([
  { name: "full_name", input: "profile-full-name", key: "full_name" },
  { name: "bio", input: "profile-bio", key: "bio" },
  { name: "website", input: "profile-website", key: "website_url" },
  { name: "x", input: "profile-x", key: "x_url" },
  { name: "linkedin", input: "profile-linkedin", key: "linkedin_url" },
  { name: "github", input: "profile-github", key: "github_url" }
]);

export function profileFormValues(profile) {
  const values = {};
  for (const field of PROFILE_FORM_FIELDS) {
    values[field.name] = (profile && profile[field.key]) || "";
  }
  return values;
}

export function profileErrorMessage(response) {
  const message = response?.data?.error?.message;
  return message ? `Not saved: ${message}` : `Not saved (HTTP ${response?.status ?? "error"}).`;
}

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
  if (data.voter.full_name && data.voter.full_name !== data.voter.display_name) {
    text("voter-full-name", data.voter.full_name);
    show("voter-full-name");
  }
  const avatar = document.getElementById("voter-avatar");
  if (avatar && /^https:\/\//.test(data.voter.avatar_url || "")) {
    avatar.src = data.voter.avatar_url;
    avatar.hidden = false;
  }
  show("voter-card");
  text("profile-sub", "");
  show("my-agent-section");

  if (data.my_agent) {
    const row = document.getElementById("my-agent-row");
    if (row) row.hidden = false;
    renderAgent(row, data.my_agent);
    show("agent-notifications-link");
    renderProfileForm(data.my_agent, data.my_agent_profile);
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

function renderProfileForm(agent, profile) {
  const form = document.getElementById("profile-form");
  if (!form) return;
  const link = document.getElementById("public-profile-link");
  if (link) link.href = `/agents/${agent.user_id}`;
  fillProfileForm(profileFormValues(profile));
  show("public-profile-section");
  form.addEventListener("submit", (event) => {
    event.preventDefault();
    saveProfile(agent.user_id);
  });
}

function fillProfileForm(values) {
  for (const field of PROFILE_FORM_FIELDS) {
    const input = document.getElementById(field.input);
    if (input) input.value = values[field.name];
  }
}

async function saveProfile(userId) {
  const status = document.getElementById("profile-form-status");
  const button = document.getElementById("profile-save");
  const fields = {};
  for (const field of PROFILE_FORM_FIELDS) {
    const input = document.getElementById(field.input);
    if (input) fields[field.name] = input.value;
  }
  setStatus(status, "Saving…", "");
  if (button) button.disabled = true;
  try {
    const response = await updateAgentProfile(userId, fields);
    if (response.ok && response.data?.data) {
      fillProfileForm(profileFormValues(response.data.data.profile));
      setStatus(status, "Saved.", "is-success");
    } else {
      setStatus(status, profileErrorMessage(response), "is-error");
    }
  } catch (error) {
    setStatus(status, `Not saved: ${error}`, "is-error");
  } finally {
    if (button) button.disabled = false;
  }
}

function setStatus(el, message, state) {
  if (!el) return;
  el.textContent = message;
  el.classList.remove("is-error", "is-success");
  if (state) el.classList.add(state);
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

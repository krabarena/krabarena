import { proposeBattleCandidate } from "./api.js";
import { getVoterToken } from "./auth-requests.js";
import { voteCandidate } from "./feed.js";

export function initBattleCandidates() {
  const root = document.querySelector("[data-battle-candidates-page]");
  if (!root) return;
  const token = getVoterToken();
  const anonEl = document.getElementById("propose-anon");
  const formEl = document.getElementById("propose-form");
  if (token) {
    if (formEl) formEl.hidden = false;
  } else if (anonEl) {
    anonEl.hidden = false;
  }

  formEl?.addEventListener("submit", async (event) => {
    event.preventDefault();
    const msg = document.getElementById("propose-msg");
    msg.textContent = "Submitting...";
    const title = document.getElementById("propose-title").value.trim();
    const rationale = document.getElementById("propose-rationale").value.trim();
    try {
      const response = await proposeBattleCandidate({ title, rationale });
      if (!response.ok) {
        const body = response.data || {};
        msg.textContent = body?.error?.message || `HTTP ${response.status}`;
        setMessageState(msg, "error");
        return;
      }
      msg.textContent = "Proposed.";
      setMessageState(msg, "success");
      window.setTimeout(() => window.location.reload(), 500);
    } catch (error) {
      msg.textContent = `Network error: ${error}`;
      setMessageState(msg, "error");
    }
  });

  document.querySelectorAll(".vote-btn").forEach((button) => {
    button.addEventListener("click", (event) => voteCandidate(button, event));
  });
}

function setMessageState(el, state) {
  el.classList.toggle("is-error", state === "error");
  el.classList.toggle("is-success", state === "success");
}

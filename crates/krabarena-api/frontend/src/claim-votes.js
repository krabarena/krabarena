import { track } from "./analytics.js";
import { castClaimVote, getClaimVotes } from "./api.js";
import { isVoterLoggedIn, setLoginReturn } from "./session.js";

export function initClaimPage() {
  const page = document.querySelector("[data-claim-page]");
  if (!page) return;
  const claimId = page.getAttribute("data-claim-id");
  if (claimId) track("claim_open", { claim_id: claimId });

  document.addEventListener("click", (event) => {
    const close = event.target.closest("[data-attachment-close]");
    if (!close) return;
    event.preventDefault();
    close.closest("details").open = false;
  });

  document.addEventListener("click", (event) => {
    if (event.target.closest("[data-stop-propagation]")) event.stopPropagation();
  });

  initClaimVotes(document.querySelector("[data-claim-votes]"));
}

export function pressedVote(responseVoteType) {
  return responseVoteType === "up" || responseVoteType === "down" ? responseVoteType : null;
}

export function voteCounts(body) {
  const data = body?.data ?? {};
  return {
    down: Number.isFinite(data.downvotes) ? data.downvotes : null,
    up: Number.isFinite(data.upvotes) ? data.upvotes : null
  };
}

export function initClaimVotes(root, { loggedIn = isVoterLoggedIn } = {}) {
  if (!root) return;
  const claimId = root.getAttribute("data-claim-id");
  const buttons = Array.from(root.querySelectorAll("[data-claim-vote]"));
  const hint = root.querySelector("[data-claim-votes-hint]");
  if (!loggedIn() && hint) hint.hidden = false;

  buttons.forEach((button) => {
    button.addEventListener("click", async (event) => {
      event.preventDefault();
      if (!loggedIn()) {
        setLoginReturn(window.location.href);
        window.location.href = `/login?return=${encodeURIComponent(window.location.pathname)}`;
        return;
      }
      buttons.forEach((b) => (b.disabled = true));
      try {
        const response = await castClaimVote({
          claimId,
          voteType: button.getAttribute("data-claim-vote")
        });
        if (!response.ok) return;
        const pressed = pressedVote(response.data?.data?.vote_type);
        buttons.forEach((b) =>
          b.setAttribute("aria-pressed", String(b.getAttribute("data-claim-vote") === pressed))
        );
        const counts = voteCounts((await getClaimVotes({ claimId })).data);
        for (const kind of ["up", "down"]) {
          const el = root.querySelector(`[data-vote-count="${kind}"]`);
          if (el && counts[kind] !== null) el.textContent = String(counts[kind]);
        }
        track("claim_vote", { claim_id: claimId, vote: pressed ?? "removed" });
      } finally {
        buttons.forEach((b) => (b.disabled = false));
      }
    });
  });
}

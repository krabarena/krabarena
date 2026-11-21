import { track } from "./analytics.js";

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
}

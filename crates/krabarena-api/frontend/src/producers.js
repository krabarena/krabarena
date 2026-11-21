// "Produced by" brand chip — touch behavior.
//
// On desktop the popover is a pure CSS :hover / :focus-within reveal and the
// chip is a plain anchor, so a click just navigates. On no-hover (touch)
// devices there is no hover, so a tap would jump straight to the producer site
// and the description/Visit affordance would never be seen. Here we make the
// first tap open the popover instead, and only let the "Visit" button inside it
// navigate.

const OPEN_CLASS = "is-open";

function closeAllExcept(keep) {
  document.querySelectorAll(`.producer-chip.${OPEN_CLASS}`).forEach((chip) => {
    if (chip !== keep) chip.classList.remove(OPEN_CLASS);
  });
}

export function initProducers() {
  const noHover = window.matchMedia("(hover: none)");

  document.addEventListener("click", (event) => {
    const chip = event.target.closest(".producer-chip");

    // Tap outside any chip closes whatever is open.
    if (!chip) {
      closeAllExcept(null);
      return;
    }

    // Hover-capable devices keep the CSS reveal + native anchor navigation.
    if (!noHover.matches) return;

    // Once open, a tap on the Visit button navigates (let the anchor through).
    if (chip.classList.contains(OPEN_CLASS) && event.target.closest(".producer-chip-pop-cta")) {
      return;
    }

    // First tap (or a tap not on Visit): reveal the popover, don't navigate.
    event.preventDefault();
    closeAllExcept(chip);
    chip.classList.add(OPEN_CLASS);
  });
}

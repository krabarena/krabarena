import { copyText } from "./clipboard.js";
import { track } from "./analytics.js";
import { showToast } from "./toast.js";

const COPY_RESET_DELAY_MS = 2400;

export function dialogCopyFeedbackState(state, messages = {}) {
  switch (state) {
    case "copying":
      return {
        label: "Copying...",
        message: "Copying command...",
        busy: true,
        isError: false
      };
    case "success":
      return {
        label: "Copied",
        message: messages.successMessage || "Command copied.",
        busy: false,
        isError: false
      };
    case "error":
      return {
        label: "Copy failed",
        message: messages.errorMessage || "Could not copy.",
        busy: false,
        isError: true
      };
    default:
      return {
        label: messages.originalLabel || "Copy Command",
        message: "",
        busy: false,
        isError: false
      };
  }
}

export function closeDialog(dialog) {
  if (!dialog) return;
  dialog.classList.add("closing");
  window.setTimeout(() => {
    dialog.classList.remove("closing");
    if (dialog.open) dialog.close();
  }, 160);
}

export function initDialogs() {
  document.addEventListener("click", async (event) => {
    const open = event.target.closest("[data-dialog-open]");
    if (open) {
      event.preventDefault();
      const dialog = document.querySelector(open.getAttribute("data-dialog-open"));
      resetDialogCopyFeedback(dialog);
      dialog?.showModal();
      return;
    }

    const close = event.target.closest("[data-dialog-close]");
    if (close) {
      event.preventDefault();
      closeDialog(close.closest("dialog"));
      return;
    }

    const copy = event.target.closest("[data-dialog-copy-target]");
    if (!copy) return;
    event.preventDefault();
    const dialog = copy.closest("dialog");
    const target = document.querySelector(copy.getAttribute("data-dialog-copy-target"));
    const status = dialogCopyStatus(copy);
    const source = copy.getAttribute("data-analytics-source");
    if (source) track("cli_install_click", { source });
    setDialogCopyFeedback(copy, status, dialogCopyFeedbackState("copying"));
    try {
      await copyText(target ? target.textContent : "");
      const feedback = dialogCopyFeedbackState("success", copyFeedbackMessages(copy));
      setDialogCopyFeedback(copy, status, feedback);
      showToast(feedback.message);
      window.setTimeout(() => closeDialog(dialog), COPY_RESET_DELAY_MS);
    } catch {
      const feedback = dialogCopyFeedbackState("error", copyFeedbackMessages(copy));
      setDialogCopyFeedback(copy, status, feedback);
      showToast(feedback.message, true);
    }
  });

  document.querySelectorAll("dialog[data-krab-dialog]").forEach((dialog) => {
    dialog.addEventListener("click", (event) => {
      if (event.target === dialog) closeDialog(dialog);
    });
    dialog.addEventListener("cancel", (event) => {
      event.preventDefault();
      closeDialog(dialog);
    });
    dialog.addEventListener("close", () => resetDialogCopyFeedback(dialog));
  });
}

function copyFeedbackMessages(copy) {
  return {
    successMessage: copy.getAttribute("data-success-message"),
    errorMessage: copy.getAttribute("data-error-message"),
    originalLabel: originalCopyLabel(copy)
  };
}

function originalCopyLabel(copy) {
  if (!copy.dataset.dialogCopyOriginalLabel) {
    copy.dataset.dialogCopyOriginalLabel = copy.textContent.trim() || "Copy Command";
  }
  return copy.dataset.dialogCopyOriginalLabel;
}

function dialogCopyStatus(copy) {
  const selector = copy.getAttribute("data-dialog-copy-status");
  if (selector) return document.querySelector(selector);
  return copy.closest(".verify-modal-actions")?.querySelector("[data-dialog-copy-status]");
}

function setDialogCopyFeedback(copy, status, feedback) {
  originalCopyLabel(copy);
  copy.textContent = feedback.label;
  copy.disabled = feedback.busy;
  copy.setAttribute("aria-busy", feedback.busy ? "true" : "false");
  copy.classList.toggle("is-copied", feedback.label === "Copied");
  copy.classList.toggle("is-error", feedback.isError);
  if (!status) return;
  status.hidden = !feedback.message;
  status.textContent = feedback.message;
  status.classList.toggle("is-error", feedback.isError);
  status.setAttribute("role", feedback.isError ? "alert" : "status");
  status.setAttribute("aria-live", feedback.isError ? "assertive" : "polite");
}

function resetDialogCopyFeedback(dialog) {
  if (!dialog) return;
  dialog.querySelectorAll("[data-dialog-copy-target]").forEach((copy) => {
    setDialogCopyFeedback(
      copy,
      dialogCopyStatus(copy),
      dialogCopyFeedbackState("idle", copyFeedbackMessages(copy))
    );
  });
}

import assert from "node:assert/strict";
import { test } from "node:test";

import { dialogCopyFeedbackState } from "./dialog.js";

test("dialog copy feedback exposes success label and message", () => {
  assert.deepEqual(
    dialogCopyFeedbackState("success", {
      successMessage: "Command copied. Paste it into your agent."
    }),
    {
      label: "Copied",
      message: "Command copied. Paste it into your agent.",
      busy: false,
      isError: false
    }
  );
});

test("dialog copy feedback exposes error label and message", () => {
  assert.deepEqual(
    dialogCopyFeedbackState("error", {
      errorMessage: "Could not copy. Select the command manually."
    }),
    {
      label: "Copy failed",
      message: "Could not copy. Select the command manually.",
      busy: false,
      isError: true
    }
  );
});

test("dialog copy feedback falls back to safe defaults", () => {
  assert.deepEqual(dialogCopyFeedbackState("copying"), {
    label: "Copying...",
    message: "Copying command...",
    busy: true,
    isError: false
  });
  assert.deepEqual(dialogCopyFeedbackState("idle"), {
    label: "Copy Command",
    message: "",
    busy: false,
    isError: false
  });
});

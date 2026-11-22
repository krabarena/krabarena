import assert from "node:assert/strict";
import { test } from "node:test";

import { PROFILE_FORM_FIELDS, profileErrorMessage, profileFormValues } from "./profile.js";

test("profileFormValues maps API profile keys onto form fields", () => {
  const values = profileFormValues({
    bio: "Builds agents",
    full_name: "Jane Doe",
    github_url: "https://github.com/jane",
    linkedin_url: null,
    website_url: "https://example.com/",
    x_url: "https://x.com/jane"
  });
  assert.deepEqual(values, {
    bio: "Builds agents",
    full_name: "Jane Doe",
    github: "https://github.com/jane",
    linkedin: "",
    website: "https://example.com/",
    x: "https://x.com/jane"
  });
});

test("profileFormValues returns empty strings without a profile", () => {
  const values = profileFormValues(null);
  assert.equal(Object.keys(values).length, PROFILE_FORM_FIELDS.length);
  assert.ok(Object.values(values).every((v) => v === ""));
});

test("profileErrorMessage surfaces the server validation message", () => {
  assert.equal(
    profileErrorMessage({
      data: { error: { message: "x.com: only x.com links are accepted here" } },
      status: 400
    }),
    "Not saved: x.com: only x.com links are accepted here"
  );
  assert.equal(profileErrorMessage({ data: "", status: 403 }), "Not saved (HTTP 403).");
});

import assert from "node:assert/strict";
import { test } from "node:test";

import { authNavState } from "./nav.js";

test("nav treats a voter token without a name as signed in", () => {
  assert.deepEqual(
    authNavState({
      voterToken: "voter-token",
      voterName: null,
      adminToken: null
    }),
    {
      isSignedIn: true,
      displayName: "User"
    }
  );
});

test("nav treats an admin token as signed in", () => {
  assert.deepEqual(
    authNavState({
      voterToken: null,
      voterName: null,
      adminToken: "admin-token"
    }),
    {
      isSignedIn: true,
      displayName: "Admin"
    }
  );
});

test("nav treats an empty session as signed out", () => {
  assert.deepEqual(
    authNavState({
      voterToken: null,
      voterName: null,
      adminToken: null
    }),
    {
      isSignedIn: false,
      displayName: "User"
    }
  );
});

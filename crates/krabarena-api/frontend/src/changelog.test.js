import assert from "node:assert/strict";
import { test } from "node:test";

import { changelogPrunePlan, changelogSentinelTargets } from "./changelog.js";

test("sentinel targets: first page has no previous", () => {
  assert.deepEqual(changelogSentinelTargets([1, 2, 3], 8), { prev: null, next: 4 });
});

test("sentinel targets: a middle window has both ends", () => {
  assert.deepEqual(changelogSentinelTargets([3, 4, 5], 8), { prev: 2, next: 6 });
});

test("sentinel targets: last page has no next", () => {
  assert.deepEqual(changelogSentinelTargets([6, 7, 8], 8), { prev: 5, next: null });
});

test("sentinel targets: empty list yields nothing", () => {
  assert.deepEqual(changelogSentinelTargets([], 8), { prev: null, next: null });
});

test("prune plan: scrolling down drops the smallest (top) page", () => {
  assert.deepEqual(changelogPrunePlan([1, 2, 3, 4], "down", 3), [1]);
});

test("prune plan: scrolling up drops the largest (bottom) page", () => {
  assert.deepEqual(changelogPrunePlan([1, 2, 3, 4], "up", 3), [4]);
});

test("prune plan: within the window drops nothing", () => {
  assert.deepEqual(changelogPrunePlan([2, 3, 4], "down", 3), []);
});

test("prune plan: ignores order and bad values", () => {
  // valid → [2, 3, 4, 5]; window 3, down → drop the smallest
  assert.deepEqual(changelogPrunePlan([4, "2", null, 3, 5, "x"], "down", 3), [2]);
});

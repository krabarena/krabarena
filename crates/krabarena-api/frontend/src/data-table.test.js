import assert from "node:assert/strict";
import { test } from "node:test";

import { tableStatusLabel, tableWindow } from "./data-table.js";

test("data table windows a page in the middle of the list", () => {
  const win = tableWindow(134, 2, 25);
  assert.deepEqual(win, {
    page: 2,
    pageCount: 6,
    start: 50,
    end: 75,
    hasPrev: true,
    hasNext: true
  });
});

test("data table clamps an out-of-range page to the last page", () => {
  const win = tableWindow(30, 99, 25);
  assert.equal(win.page, 1);
  assert.equal(win.start, 25);
  assert.equal(win.end, 30);
  assert.equal(win.hasNext, false);
  assert.equal(win.hasPrev, true);
});

test("data table treats an empty list as a single empty page", () => {
  const win = tableWindow(0, 0, 25);
  assert.deepEqual(win, {
    page: 0,
    pageCount: 1,
    start: 0,
    end: 0,
    hasPrev: false,
    hasNext: false
  });
});

test("data table first page has no previous", () => {
  const win = tableWindow(10, 0, 25);
  assert.equal(win.hasPrev, false);
  assert.equal(win.hasNext, false);
  assert.equal(win.end, 10);
});

test("data table status label is 1-based and hides when empty", () => {
  assert.equal(tableStatusLabel(tableWindow(134, 0, 25), 134), "1–25 of 134");
  assert.equal(tableStatusLabel(tableWindow(0, 0, 25), 0), "");
});

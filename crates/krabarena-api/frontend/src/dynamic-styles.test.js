import assert from "node:assert/strict";
import { test } from "node:test";

import { initDynamicStyles } from "./dynamic-styles.js";

test("claims wins pie data builds a conic-gradient class", () => {
  const rules = [];
  const pieClasses = new Set();
  const pie = {
    classList: { add: (className) => pieClasses.add(className) },
    querySelectorAll: () => [
      { dataset: { winsBg: "#dea584", winsCount: "3" } },
      { dataset: { winsBg: "#e34c26", winsCount: "1" } }
    ]
  };
  const previousDocument = globalThis.document;
  globalThis.document = {
    querySelectorAll: (selector) => (selector === "[data-wins-pie]" ? [pie] : []),
    createElement: () => ({
      setAttribute: () => {},
      append: (rule) => rules.push(rule)
    }),
    head: { append: () => {} }
  };

  try {
    initDynamicStyles();
  } finally {
    globalThis.document = previousDocument;
  }

  assert.equal(pieClasses.size, 1);
  assert.match(
    rules.join("\n"),
    /background: conic-gradient\(#dea584 0\.0000% 75\.0000%, #e34c26 75\.0000% 100\.0000%\)/
  );
});

test("claim hero race bars and frontier dots position from data attributes", async () => {
  // `initDynamicStyles` memoizes its `<style>` element at module scope
  // (the same singleton production relies on to dedupe rules across a
  // real page). A second call against the statically-imported module
  // from the test above would silently no-op — the `<style>` element
  // is already created and captured by that test's `rules` array. Load
  // a fresh module instance (unique specifier → fresh module scope) so
  // this test gets its own singleton.
  const { initDynamicStyles } = await import(`./dynamic-styles.js?test-instance=${Math.random()}`);
  const rules = [];
  const barClasses = new Set();
  const dotClasses = new Set();
  const bar = { dataset: { barWidth: "42.5" }, classList: { add: (c) => barClasses.add(c) } };
  const overWidthBar = {
    dataset: { barWidth: "150" },
    classList: { add: (c) => barClasses.add(c) }
  };
  const dot = { dataset: { x: "12", y: "88" }, classList: { add: (c) => dotClasses.add(c) } };

  const previousDocument = globalThis.document;
  globalThis.document = {
    querySelectorAll: (selector) => {
      if (selector === "[data-bar-width]") return [bar, overWidthBar];
      if (selector === "[data-x]") return [dot];
      if (selector === "[data-y]") return [dot];
      return [];
    },
    createElement: () => ({
      setAttribute: () => {},
      append: (rule) => rules.push(rule)
    }),
    head: { append: () => {} }
  };

  try {
    initDynamicStyles();
  } finally {
    globalThis.document = previousDocument;
  }

  assert.equal(barClasses.size, 2);
  assert.equal(dotClasses.size, 2);
  const css = rules.join("\n");
  assert.match(css, /width: 42\.5%/);
  // Out-of-range percentages clamp to 0..100 so a malformed value
  // can't push a bar/dot off the card.
  assert.match(css, /width: 100%/);
  assert.match(css, /left: 12%/);
  assert.match(css, /bottom: 88%/);
});

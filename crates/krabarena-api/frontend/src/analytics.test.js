import assert from "node:assert/strict";
import { test } from "node:test";

import { initAnalytics, normalizeString, pageLocation, pagePath } from "./analytics.js";

test("normalizeString trims, collapses whitespace, and removes control characters", () => {
  assert.equal(normalizeString("  Hello\n\tKrab\u0007Arena  "), "Hello Krab Arena");
});

test("normalizeString rejects unsupported values and empty strings", () => {
  assert.equal(normalizeString({ secret: "value" }), null);
  assert.equal(normalizeString(" \n\t "), null);
});

test("normalizeString limits event parameter length", () => {
  const result = normalizeString("x".repeat(200));
  assert.equal(result.length, 120);
});

test("pageLocation preserves query parameters for UTM attribution", () => {
  globalThis.window = {
    location: {
      origin: "https://krabarena.com",
      pathname: "/battles/jquery-vs-react",
      search: "?utm_source=github&utm_medium=issue"
    }
  };

  assert.equal(
    pageLocation(),
    "https://krabarena.com/battles/jquery-vs-react?utm_source=github&utm_medium=issue"
  );
  assert.equal(pagePath(), "/battles/:battle");
});

test("GA config page_location includes UTM parameters", () => {
  let appendedScript = null;
  globalThis.window = {
    location: {
      origin: "https://krabarena.com",
      pathname: "/battles/jquery-vs-react",
      search: "?utm_source=github&utm_medium=issue"
    },
    dataLayer: undefined
  };
  globalThis.document = {
    title: "jQuery vs React — KrabArena",
    querySelector(selector) {
      if (selector === 'meta[name="krab-ga-id"]') {
        return { getAttribute: () => "G-TEST123" };
      }
      if (selector === 'meta[name="krab-analytics-title"]') {
        return { getAttribute: () => "Battle — KrabArena" };
      }
      return null;
    },
    createElement() {
      return { dataset: {} };
    },
    head: {
      appendChild(script) {
        appendedScript = script;
      }
    }
  };

  initAnalytics();

  const configCall = window.dataLayer.find((entry) => Array.from(entry)[0] === "config");
  assert.equal(Array.from(configCall)[2].page_location, pageLocation());
  assert.equal(Array.from(configCall)[2].page_path, "/battles/:battle");
  assert.equal(appendedScript.dataset.krabGa, "G-TEST123");
});

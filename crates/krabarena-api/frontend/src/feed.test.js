import assert from "node:assert/strict";
import { test } from "node:test";

import {
  closeTagSuggestions,
  homeFeedPageUrl,
  homeFeedWindowPlan,
  isHomeFeedResponseErrorIgnorable,
  isTagSearchTarget
} from "./feed.js";
import { XHR_STATUS } from "./http-status.js";
import { buildTestPath, buildTestUrl } from "./test-helpers/url.js";

test("home feed ignores aborted/aborted-like response errors", () => {
  assert.equal(
    isHomeFeedResponseErrorIgnorable({
      xhr: { status: XHR_STATUS.ABORTED },
      requestConfig: { path: buildTestPath({ params: { tag: "e2e" } }) }
    }),
    true
  );
});

test("home feed surfaces real errors for non-fragments", () => {
  assert.equal(
    isHomeFeedResponseErrorIgnorable({
      xhr: { status: 500 },
      requestConfig: { path: buildTestPath({ params: { page: 2, tab: "battles" } }) }
    }),
    false
  );
});

test("home feed ignores errors from tag suggestion requests", () => {
  assert.equal(
    isHomeFeedResponseErrorIgnorable({
      xhr: { status: 500 },
      requestConfig: { path: buildTestPath({ path: "/fragments/home/tags", params: { q: "ad" } }) }
    }),
    true
  );
});

test("home feed closes tag suggestions and resets expanded state", () => {
  const attrs = {};
  const suggestions = { hidden: false };
  const input = {
    setAttribute(name, value) {
      attrs[name] = value;
    }
  };

  assert.equal(closeTagSuggestions(suggestions, input), true);
  assert.equal(suggestions.hidden, true);
  assert.equal(attrs["aria-expanded"], "false");
});

test("home feed keeps tag suggestions open for tag search targets only", () => {
  assert.equal(
    isTagSearchTarget({
      closest(selector) {
        return selector === ".tag-search" ? {} : null;
      }
    }),
    true
  );
  assert.equal(
    isTagSearchTarget({
      closest() {
        return null;
      }
    }),
    false
  );
});

test("home feed window keeps only the configured latest pages", () => {
  assert.deepEqual(homeFeedWindowPlan([1, 2, 3, 4], 3), {
    removed: [1],
    firstRemaining: 2,
    omittedPage: 1
  });
  assert.deepEqual(homeFeedWindowPlan([2, 3, 4], 3), {
    removed: [],
    firstRemaining: 2,
    omittedPage: 1
  });
});

test("home feed page URL preserves query state and normalizes page one", () => {
  const pageTwoUrl = buildTestUrl({
    baseURL: "http://127.0.0.1:3000",
    params: {
      page: 2,
      q: "foo",
      sort: "new",
      status: "open",
      tab: "claims",
      tag: "rust,ai"
    }
  });
  const pageFourPath = buildTestPath({
    params: {
      page: 4,
      q: "foo",
      sort: "new",
      status: "open",
      tab: "claims",
      tag: "rust,ai"
    }
  });
  assert.equal(homeFeedPageUrl(4, pageTwoUrl), pageFourPath);
  const filteredPageTwoUrl = buildTestUrl({
    baseURL: "http://127.0.0.1:3000",
    params: { page: 2, q: "foo", tab: "claims" }
  });
  const filteredPageOnePath = buildTestPath({
    params: { q: "foo", tab: "claims" }
  });
  assert.equal(homeFeedPageUrl(1, filteredPageTwoUrl), filteredPageOnePath);
});

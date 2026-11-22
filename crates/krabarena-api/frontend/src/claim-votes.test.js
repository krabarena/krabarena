import assert from "node:assert/strict";
import { test } from "node:test";

import { pressedVote, voteCounts } from "./claim-votes.js";

test("pressedVote maps the vote endpoint result to the pressed button", () => {
  assert.equal(pressedVote("up"), "up");
  assert.equal(pressedVote("down"), "down");
  assert.equal(pressedVote("removed"), null);
  assert.equal(pressedVote(undefined), null);
});

test("voteCounts reads the counts envelope and ignores missing values", () => {
  assert.deepEqual(voteCounts({ data: { downvotes: 2, upvotes: 5 } }), { down: 2, up: 5 });
  assert.deepEqual(voteCounts({}), { down: null, up: null });
  assert.deepEqual(voteCounts(null), { down: null, up: null });
});

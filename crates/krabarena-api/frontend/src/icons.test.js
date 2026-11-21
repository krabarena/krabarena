import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { beforeEach, test } from "node:test";

import { iconMarkup, iconSpriteUrl } from "./icons.js";

beforeEach(() => {
  global.document = {
    querySelector(selector) {
      if (selector !== 'meta[name="krab-icon-sprite"]') return null;
      return {
        getAttribute() {
          return "/static/dist/assets/krab-icons.abc123.svg";
        }
      };
    }
  };
});

test("iconSpriteUrl reads the configured sprite path", () => {
  assert.equal(iconSpriteUrl(), "/static/dist/assets/krab-icons.abc123.svg");
});

test("iconMarkup renders decorative and labelled SVG use references safely", () => {
  assert.equal(iconMarkup("copy"), '<span class="icon icon-copy" aria-hidden="true"></span>');

  assert.equal(
    iconMarkup('x"><script>', 'Close "dialog"'),
    '<span class="icon icon-x&quot;&gt;&lt;script&gt;" role="img" aria-label="Close &quot;dialog&quot;"></span>'
  );
});

test("monochrome brand icons keep explicit fill inside the sprite", () => {
  const sprite = readFileSync(resolve(import.meta.dirname, "icons/krab-icons.svg"), "utf8");

  assert.match(sprite, /<symbol id="icon-github"[^>]*><path fill="currentColor"/);
  assert.match(sprite, /<symbol id="icon-discord"[^>]*><path fill="currentColor"/);
});

test("line icons keep explicit stroke inside the sprite", () => {
  const sprite = readFileSync(resolve(import.meta.dirname, "icons/krab-icons.svg"), "utf8");

  assert.match(sprite, /<symbol id="icon-search"[^>]*><path fill="none" stroke="currentColor"/);
  assert.match(sprite, /<symbol id="icon-search"[^>]*>.*<circle fill="none" stroke="currentColor"/);
  assert.match(sprite, /<symbol id="icon-copy"[^>]*><rect fill="none" stroke="currentColor"/);
});

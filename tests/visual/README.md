# Visual / mobile-layout tests

Playwright check that the public pages **fit on phones** — no horizontal
page overflow and no clipped content (the 0.21.3 regression where the
home feed cut "active" → "act"). It doubles as a screenshot capturer:
every run writes one PNG per page × width to [`screenshots/`](./screenshots),
which are committed so layout changes show up as image diffs in PRs.

## Run

```sh
cd tests/visual
npm install                 # installs playwright + chromium (postinstall)
npm test                    # against the local dev server (http://127.0.0.1:3013)

BASE_URL=https://krabarena.com npm test   # against prod
```

Start a local server first if testing locally — see the repo root
`AGENTS.md` ("Local development against prod data"); run it on `PORT=3013`
(or point `BASE_URL` at whatever port you use).

## What it checks

For `/`, a battle page, `/arena`, `/agents` at **320 / 360 / 390px**:

- `document.body.scrollWidth <= clientWidth` (no horizontal page scroll),
- no element clips its own text via a non-scrollable `overflow`
  (intentional ellipsis on `.name` / `.cell-clamp` / `.th-label` is
  excluded).

Exit code is non-zero on any failure, so it can gate CI. The Rust
counterpart `crates/krabarena-api/tests/feed_e2e.rs` asserts the feed is
non-empty; this asserts it *fits*.

## Updating the reference screenshots

`npm test` overwrites `screenshots/`. Review the diff, then commit the
updated PNGs alongside the CSS/template change that caused them.

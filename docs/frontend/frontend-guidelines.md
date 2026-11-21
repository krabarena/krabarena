# Frontend Guidelines

These rules describe how to write frontend code in KrabArena now.

## Build

- Frontend assets are built with Vite from `crates/krabarena-api/frontend`.
- JavaScript stays plain JavaScript. Do not introduce TypeScript.
- HTML templates are not auto-formatted. Keep Askama markup readable by hand.
- `NODE_ENV=development` is used for development builds.
- `NODE_ENV=production` is used for production builds.
- Production assets are fingerprinted, minified, and precompressed with gzip.
- CI runs `npm ci`, lint, format check, unit tests, and a production build
  when frontend sources, templates, static assets, frontend docs, Dockerfile,
  or workflow files change.

## Template Structure

- `templates/layouts`: page shells.
- `templates/pages`: full browser-facing pages.
- `templates/components`: reusable Askama fragments.
- `templates/partials`: server-rendered fragments for HTMX and feed updates.
- `templates/md`: markdown responses.
- `templates/og`: OpenGraph rendering templates.

Browser-facing routes render HTML. REST and internal API routes keep JSON or service-specific response contracts.

## HTML Semantics

- Use semantic elements before generic `div`: `header`, `nav`, `main`, `section`, `article`, `aside`, `footer`, `dl`, `figure`, and `time`.
- Every `section` that is not self-evident needs a heading or `aria-labelledby`.
- Breadcrumbs use `nav aria-label="Breadcrumb"` with an ordered list.
- URL-backed feed/page switches use `nav aria-label`, not ARIA tab roles.
- Cards that represent independent items use `article`; metadata belongs in `header`; actions belong in `footer`.
- Facts use `dl`; unordered metadata uses `ul`.
- Tables need `caption`, `scope="col"`, and `scope="row"`.
- Inputs need labels. Related controls need `fieldset` and `legend`.
- Dynamic status messages use `aria-live="polite"`.

## CSS

KrabArena uses CUBE CSS in the bundled stylesheet:

- `tokens`: design values and compatibility aliases.
- `base`: element defaults.
- `composition`: layout primitives with `l-*` names.
- `utilities`: single-purpose helpers with readable `u-*` names.
- `components`: reusable blocks with `c-*` names.
- `exceptions`: explicit one-off rules for backend-fed CSS custom properties hydrated from `data-*` attributes into generated classes, plus unavoidable browser quirks.

Class rules:

- Use `l-*` for layout primitives: `l-stack`, `l-cluster`, `l-repel`, `l-auto-grid`, `l-center`.
- Use `u-*` only for single-purpose utilities: `u-visually-hidden`, `u-muted`, `u-text-right`, `u-mono`, `u-wrap`.
- Use `c-*` for components and fragments.
- Use `is-*` and `has-*` for state.
- Use `data-*` for JavaScript hooks.

Do not add inline `style` attributes for visual rules. Backend values may only be exposed as explicit `data-*` attributes and hydrated by bundled frontend modules into generated classes with approved CSS custom properties.

## JavaScript

- UI behavior lives in `frontend/src` modules.
- Templates pass state with `data-*` attributes.
- Do not use inline handlers such as `onclick`, `onsubmit`, `onchange`, or `oninput`, or `javascript:` URLs. The Content-Security-Policy (`src/web/security_headers.rs`) has no `'unsafe-inline'` in `script-src` plus `script-src-attr 'none'`, so the browser blocks them anyway.
- Do not add inline scripts to templates. The only allowed inline `<script>` is a non-executed data block (JSON-LD), and it must carry `nonce="{{ meta.csp_nonce }}"` — the per-response CSP nonce. Hand-written HTML under `static/` (e.g. `static/demo/`) gets no nonce, so it must have no inline script at all. `npm run lint:templates` enforces all of this.
- Do not add direct `/static/*.js` script links to browser-facing templates.
- `session.js` is the only module that reads or writes auth keys in `localStorage`.
- HTMX should be used for server-rendered dynamic fragments where it keeps the UI simpler than custom JavaScript.
- REST-backed frontend flows use endpoint wrappers over the shared Axios client.

## Icons

- Use the SVG sprite and `PageMeta` icon helpers for UI icons.
- Decorative icons are `aria-hidden`.
- Meaningful icon-only controls need `aria-label`.
- Raw SVG in templates is limited to brand/logo art and OG templates.

## Forms, Tables, Dialogs, Toasts

- Forms use labelled controls, visible focus states, and `aria-live="polite"` for async status.
- Tables use semantic headers and sortable headers only when the table-sort module should attach.
- Dialogs use native `dialog`, a labelled heading, a close button, and `data-dialog-*` wiring.
- Toasts are created through the bundled toast module and must never include secrets, tokens, or raw backend payloads.

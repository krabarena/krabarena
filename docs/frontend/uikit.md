# KrabArena UI-kit

This is the frontend contract for browser-facing KrabArena pages.

## Foundations

- Fonts: Google Fonts are the primary delivery path.
- Body text uses Inter.
- Code, commands, and compact numeric labels use JetBrains Mono.
- VT323 is reserved for established brand display headings.
- Colors, spacing, radii, shadows, z-index values, and breakpoints come from `frontend/src/styles/tokens.css`.
- Pages load CSS and JavaScript through the Vite bundle. Do not add direct `/static/style.css`, `/static/theme-forum.css`, or standalone script links to templates.
- Frontend code is plain JavaScript. Do not add TypeScript or HTML auto-formatting.

## CSS Layers

The bundle uses CUBE CSS layers in this order:

- `tokens`: design tokens and old variable aliases.
- `base`: document defaults and baseline selectors.
- `composition`: layout primitives such as stack, cluster, repel, grid, center, reel, and sidebar.
- `utilities`: small single-purpose helpers.
- `components`: reusable UI blocks.
- `exceptions`: deliberate one-off rules for backend-supplied CSS custom properties hydrated from `data-*` attributes into generated classes, plus unavoidable browser quirks.

## Naming

- Composition classes use `l-*`: `l-flow`, `l-stack`, `l-cluster`, `l-repel`, `l-auto-grid`, `l-center`.
- Utility classes use readable `u-*`: `u-visually-hidden`, `u-muted`, `u-text-right`, `u-mono`, `u-wrap`, `u-mt-sm`.
- Component classes use `c-*`: `c-showcase-card`, `c-radio-card`, `c-form-status`, `c-share-list`, `c-breadcrumb-list`.
- State classes use `is-*` and `has-*`.
- JavaScript hooks use `data-*`, never CSS class names.

## Components

- Buttons: use `cta-btn` for large primary CTAs, `cta-btn alt` for secondary CTAs, and `btn`, `btn-primary`, or `btn-ghost` for compact actions.
- Forms: every input, select, and textarea needs a visible label or a visually hidden label. Related controls use `fieldset` and `legend`.
- Cards: independent content items use `article`; card titles go in `header`; actions go in `footer`.
- Tabs: URL-backed page switches are navigation, not ARIA tabs. Use `nav aria-label="..."` and `aria-current="page"` on the active link.
- Badges: use `status-pill`, `pill verify`, `pill refute`, `pill-brand`, or `pill-muted`.
- Tables: use `caption`, `scope="col"`, and `scope="row"`. Add `sortable` only when the bundled sort behavior should attach.
- Toasts: use the bundled toast module. Status text must not include secrets or raw payloads.
- Dialogs: use native `dialog`, a labelled heading, a close button, and `data-krab-dialog` / `data-dialog-*` attributes.
- Empty states: use a heading, one short explanation, and one primary recovery action.
- Error states: keep backend details out of the UI; show a safe title, safe description, and a recovery action.
- Icons: use `PageMeta` icon helpers or the frontend sprite helper. Raw UI SVG in templates is only allowed for brand/logo art and OG templates.
- HTMX is the default for server-rendered fragments. Keep no-JS fallbacks as real links or forms.

## States

Every reusable component must cover the states it can enter:

- default
- hover
- focus-visible
- disabled
- loading
- empty
- error

Do not rely on color alone to communicate state. Text and icons must remain readable on hover and disabled backgrounds.

## Dynamic Values

Inline `style` attributes are not allowed in browser-facing templates. Backend values for visualizations must be passed through explicit `data-*` attributes and hydrated by bundled frontend code into generated classes with approved CSS custom properties, such as verification bar proportions, logo colors, and wins chart widths.

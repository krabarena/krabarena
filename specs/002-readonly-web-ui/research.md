# Research: Readonly Web UI

**Date**: 2026-04-03
**Branch**: `002-readonly-web-ui`

## HTML Templating

**Decision**: Askama (compile-time HTML templates)
**Rationale**: Templates are type-checked at compile time —
aligns with constitution Principle IV. Zero runtime overhead.
Templates live alongside Rust code. Jinja2-like syntax is
familiar.
**Alternatives considered**:
- Tera: Runtime template engine, no compile-time checking.
- Maud: Rust macro DSL, less readable for designers.
- Handlebars: Runtime, JavaScript heritage, no type safety.

## Styling Approach

**Decision**: Single CSS file with CSS custom properties, no
build step
**Rationale**: Minimal complexity. Dark theme via CSS variables.
Responsive via media queries. No bundler, no npm, no node_modules.
Keeps the project pure Rust.
**Alternatives considered**:
- Tailwind CSS: Requires Node.js build step — violates simplicity.
- CSS-in-Rust macros: Poor designer ergonomics.

## Static File Serving

**Decision**: tower-http ServeDir (already a dependency)
**Rationale**: Already in Cargo.toml. Serve `/static/` directory.
Works with Vercel deployment.

## New API Endpoints

**Decision**: Add two internal query functions, not public API
endpoints
**Rationale**: The leaderboard and contributor profile are UI-only
views. Rather than adding formal `/v1/` endpoints, the web page
handlers query the database directly using service functions. This
avoids expanding the public API contract for display-only features.
If CLI or agents need these later, promote to formal endpoints.

## Page Routing

**Decision**: Serve HTML pages under the root path (`/`, `/holes/`,
etc.) while API remains under `/v1/`
**Rationale**: Clean URL separation. No conflict with existing API.
Visitors get human-friendly URLs.

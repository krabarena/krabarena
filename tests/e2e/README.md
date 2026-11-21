# KrabArena browser E2E tests

This directory is a standalone Python / UV project for browser-based E2E tests.
Production smoke tests will be implemented here as a marked subset of the
broader E2E suite.

## Scope

- The default production target is `https://krabarena.com`.
- The target URL can be overridden for local or testbed E2E runs.
- Tests use Python Playwright through UV-managed dependencies.
- CloakBrowser is available as a Python dependency only.
- CloakBrowser MCP must not be used for browser navigation, execution, or
  pass/fail decisions.
- No Node, npm, or npx Playwright project is required for this suite.

## Setup

```sh
cd tests/e2e
uv sync --locked
uv run playwright install --with-deps chromium
```

## Commands

Run all E2E tests:

```sh
uv run pytest
```

Run only production smoke tests after smoke scenarios are added:

```sh
uv run pytest -m smoke
```

Run against a non-default URL:

```sh
uv run pytest --base-url http://127.0.0.1:3001
```

The same override can be provided through the environment:

```sh
KRAB_E2E_BASE_URL=http://127.0.0.1:3001 uv run pytest
```

Lint and format:

```sh
uv run ruff check .
uv run ruff format .
```

## Artifacts

The shared pytest fixtures write runtime artifacts under:

```text
tests/e2e/.artifacts/
```

For failed Playwright tests, the foundation records:

- native Playwright trace `.zip` files;
- screenshots;
- `failure.json`;
- `error.txt`;
- aggregate `results.json`.

Trace files are intended to be opened with Playwright Trace Viewer.

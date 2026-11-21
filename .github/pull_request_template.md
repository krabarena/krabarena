## What and why

## How it was tested

## Checklist

- [ ] Title follows Conventional Commits (`type(scope): description`)
- [ ] `cargo fmt --all -- --check`, `cargo clippy --workspace -- -D warnings` and `cargo test --workspace` pass
- [ ] Bug fixes include a test that fails without the fix
- [ ] New migrations are idempotent and don't edit merged ones
- [ ] UI changes update `tests/e2e/` page objects, or the PR says why they're unaffected

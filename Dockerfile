# Multi-stage build with cargo-chef for cross-run dependency caching.
#
# Why: BuildKit's `--mount=type=cache,target=/app/target` is ephemeral
# per runner. On GitHub Actions every run starts with an empty cache
# mount, so the previous Dockerfile recompiled axum/tokio/sqlx/utoipa
# from scratch every build (~6min cold). cargo-chef sidesteps this by
# splitting the build into a deps-only "cook" layer that's keyed on
# the *content* of recipe.json (a digest of Cargo.toml/Cargo.lock).
# Code-only PRs hit the cooked layer from `cache-from: type=gha` in
# .github/workflows/build-push.yml and only recompile workspace
# members.
#
# Reference: https://github.com/LukeMathWalker/cargo-chef

# Stage 1: chef base — has cargo-chef preinstalled, saves ~1min vs
# installing it ourselves on every run.
FROM lukemathwalker/cargo-chef:latest-rust-1 AS chef
WORKDIR /app

# Stage 2: planner — emits recipe.json fingerprinting the workspace's
# external deps. Source changes invalidate this layer, but recipe.json
# only changes when Cargo.toml deps change, so the next stage's cache
# still hits.
FROM chef AS planner
COPY Cargo.toml Cargo.lock ./
COPY crates/ crates/
RUN cargo chef prepare --recipe-path recipe.json

# Stage 2b: frontend — builds the Vite bundle once and copies only the
# generated dist into the runtime image. `node_modules` never crosses
# the stage boundary.
FROM node:22-bookworm-slim AS frontend
WORKDIR /app/crates/krabarena-api/frontend
COPY crates/krabarena-api/frontend/package.json crates/krabarena-api/frontend/package-lock.json ./
RUN npm ci
COPY crates/krabarena-api/frontend/ ./
ENV NODE_ENV=production
RUN npm run build

# Stage 3: builder — cooks deps from recipe (the win), then compiles
# workspace.
FROM chef AS builder
# `curl` is needed at build time by `utoipa-swagger-ui`'s build.rs
# (since 0.8.24) — it shells out to download Swagger UI's static
# assets at compile time. Without it, `cargo build --release` fails
# with `failed to download Swagger UI: curl command not found`.
RUN apt-get update && apt-get install -y pkg-config libssl-dev curl && rm -rf /var/lib/apt/lists/*

COPY --from=planner /app/recipe.json recipe.json
# The cached-deps layer. Re-runs only when recipe.json content
# changes. `--bin krabarena-api` scopes cooking to just this binary's
# transitive deps (skips krab-cli's clap-heavy tree).
RUN cargo chef cook --release --recipe-path recipe.json --bin krabarena-api

# Now the actual workspace build. Deps are already compiled in
# target/release/deps from the cook step; cargo's incremental
# fingerprinting reuses them.
COPY Cargo.toml Cargo.lock ./
COPY crates/ crates/
COPY install.sh ./
COPY CHANGELOG.md ./
# docs/ is `include_str!`-ed by krabarena-shared (the proxy-spec doc
# is served at `GET /docs/proxy-spec` at runtime). Must be in the
# build context or the release compile fails with
# "couldn't read ../../../docs/proxy-spec.md".
COPY docs/ docs/

RUN cargo build --release --bin krabarena-api && \
    cp /app/target/release/krabarena-api /usr/local/bin/krabarena-api

FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y ca-certificates libssl3 && rm -rf /var/lib/apt/lists/*

COPY --from=builder /usr/local/bin/krabarena-api /usr/local/bin/krabarena-api
COPY crates/krabarena-api/static/ /app/static/
COPY --from=frontend /app/crates/krabarena-api/static/dist/ /app/static/dist/
COPY crates/krabarena-api/templates/ /app/templates/
# routes/docs.rs reads docs/<stem>.md from the working directory
# at request time, so the runtime image needs the docs tree.
COPY docs/ /app/docs/

WORKDIR /app
ENV NODE_ENV=production
ENV LISTEN_ADDR=0.0.0.0:8080
EXPOSE 8080

CMD ["krabarena-api"]

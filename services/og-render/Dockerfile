# KrabArena OG-card render microservice.
#
# Based on the official Playwright image, which ships a matching
# headless Chromium + every system library it needs (the #1 source of
# "works locally, 500s in the container" bugs when you try to install
# Chromium yourself on a slim base). Pin the tag to the same Playwright
# version as package.json so the bundled browser and the npm package
# never drift.
#
# Local Docker test (mandatory per AGENTS.md — this is a service
# container): see services/og-render/local-test.sh.
FROM mcr.microsoft.com/playwright:v1.55.1-jammy

ENV NODE_ENV=production \
    PORT=8080

WORKDIR /app

# Install deps first for layer caching. The Playwright npm package's
# browsers are already present in the base image, so we skip the
# post-install download.
COPY package.json package-lock.json* ./
ENV PLAYWRIGHT_SKIP_BROWSER_DOWNLOAD=1
RUN npm install --omit=dev --no-audit --no-fund

COPY server.js ./

# Drop to the non-root user the Playwright image provides.
USER pwuser

EXPOSE 8080

CMD ["node", "server.js"]

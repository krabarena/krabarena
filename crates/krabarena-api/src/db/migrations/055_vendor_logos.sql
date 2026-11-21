-- 055_vendor_logos.sql — move the hand-curated vendor→logo map out of
-- Rust (`solution_names::VENDOR_PATTERNS`) into a data-driven Neon table.
--
-- Backward compatible: a NEW table only; no ALTER on existing tables, so
-- no `0A000 cached plan` recycle needed. The app reads this on the
-- battle-solutions backfill path; render still reads battles.solutions.
--
-- `pattern` is a case-insensitive regex matched against the solution name
-- (same semantics as the old const); `sort_order` preserves first-match
-- precedence. Editable at runtime (admin), so new competitors get logos
-- without a deploy.

CREATE TABLE IF NOT EXISTS vendor_logos (
    id          BIGSERIAL PRIMARY KEY,
    pattern     TEXT NOT NULL,
    domain      TEXT NOT NULL,
    sort_order  INTEGER NOT NULL DEFAULT 100,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (pattern)
);

-- Seed with the 47 rows that lived in VENDOR_PATTERNS (first-match order).
INSERT INTO vendor_logos (pattern, domain, sort_order) VALUES
  ('^anthropic\s*[·•]', 'anthropic.com', 0),
  ('^claude code', 'anthropic.com', 1),
  ('^claude\s', 'anthropic.com', 2),
  ('^claude$', 'anthropic.com', 3),
  ('codex cli', 'openai.com', 4),
  ('openai operator', 'openai.com', 5),
  ('^openai', 'openai.com', 6),
  ('^vertex', 'cloud.google.com', 7),
  ('^gemini', 'google.com', 8),
  ('^google\s*[·•]', 'google.com', 9),
  ('^google$', 'google.com', 10),
  ('^groq\s*[·•]', 'groq.com', 11),
  ('^deepseek\s*[·•]', 'deepseek.com', 12),
  ('^z\.ai\s*[·•]', 'z.ai', 13),
  ('^moonshot\s*[·•]', 'moonshot.cn', 14),
  ('^alibaba\s*[·•]', 'qwenlm.ai', 15),
  ('^qwen', 'qwenlm.ai', 16),
  ('^kimi', 'moonshot.cn', 17),
  ('^keenable\b', 'keenable.ai', 18),
  ('^exa\b', 'exa.ai', 19),
  ('^tavily\b', 'tavily.com', 20),
  ('^you\.com\b|^you_com\b', 'you.com', 21),
  ('^brightdata\b|^bright data\b', 'brightdata.com', 22),
  ('^parallel\b', 'parallel.ai', 23),
  ('^browserless\b', 'browserless.io', 24),
  ('^lightpanda\b', 'lightpanda.io', 25),
  ('^selenium\b', 'selenium.dev', 26),
  ('^chromedp', 'chromedp.io', 27),
  ('^clickhouse', 'clickhouse.com', 28),
  ('^databricks', 'databricks.com', 29),
  ('^snowflake', 'snowflake.com', 30),
  ('^sambanova', 'sambanova.ai', 31),
  ('^duckdb\b', 'duckdb.org', 32),
  ('^pandas-pyarrow|^pandas\b', 'pandas.pydata.org', 33),
  ('^polars\b', 'pola.rs', 34),
  ('^go\b', 'go.dev', 35),
  ('^native_rust', 'rust-lang.org', 36),
  ('^native_python', 'python.org', 37),
  ('^native_c', 'isocpp.org', 38),
  ('^python$', 'python.org', 39),
  ('^rust$', 'rust-lang.org', 40),
  ('^typescript$', 'typescriptlang.org', 41),
  ('^javascript$', 'javascript.com', 42),
  ('^redis_', 'redis.io', 43),
  ('^postgres_', 'postgresql.org', 44),
  ('^sqlite_', 'sqlite.org', 45),
  ('^v0\b|^vercel\b', 'v0.dev', 46)
ON CONFLICT (pattern) DO NOTHING;

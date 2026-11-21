-- 076_vendor_logos_webiq_amazon.sql — logos for the two web-search
-- providers added to the web-access battle proxy: Microsoft Web IQ and
-- Amazon (Bedrock AgentCore) Web Search.
--
-- INSERT only into vendor_logos (no ALTER), idempotent via ON CONFLICT.

INSERT INTO vendor_logos (pattern, domain, sort_order) VALUES
  ('^(microsoft\s*)?web\s*iq\b|^webiq\b', 'microsoft.com', 47),
  ('^amazon\b|^aws\b|^agentcore\b|^bedrock agentcore\b', 'aws.amazon.com', 48)
ON CONFLICT (pattern) DO NOTHING;

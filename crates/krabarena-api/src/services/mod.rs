pub mod activity_service;
pub mod agentic_reviewer;
pub mod artifact_service;
pub mod auth_service;
pub mod avatar_service;
pub mod aws_sigv4;
pub mod battle_candidates_service;
pub mod battle_review_service;
pub mod battle_service;
pub mod battle_summarizer;
pub mod benchmark_labeler;
pub mod captcha_service;
pub mod claim_media;
pub mod claim_service;
pub mod claim_visibility;
pub mod cms_service;
pub mod comment_service;
pub mod config_service;
pub mod crypto_networks;
pub mod dedup_judge;
pub mod discord_monitor;
pub mod email_domains;
pub mod email_layout;
#[cfg(test)]
pub mod email_preview;
pub mod email_service;
pub mod email_validation;
pub mod embedding_service;
pub mod format_metric;
pub mod gemini_audit;
pub mod gemini_service;
pub mod image_service;
pub mod logo_assets;
pub mod mentions;
pub mod moderation_policy;
pub mod notification_email;
pub mod og_image;
pub mod payout_service;
pub mod presentation_check;
pub mod presentation_gen;
#[allow(dead_code, clippy::too_many_arguments)]
pub mod proxy_billing;
pub mod proxy_service;
pub mod review_service;
pub mod review_tools;
pub mod search_index;
pub mod search_service;
pub mod slug_resolver;
pub mod solution_names;
pub mod tag_service;
pub mod temperature;
pub mod ua_classifier;
pub mod usage_stats;
pub mod user_log;
pub mod verification_review_service;
pub mod verification_service;
pub mod vote_service;
pub mod voter_service;
pub mod wallet_service;
pub mod webhook_dispatch;
pub mod webhook_payloads;
pub mod zoo;
pub mod zoo_feed;

#[cfg(test)]
mod auth_db_tests;
#[cfg(test)]
mod battle_candidates_db_tests;
#[cfg(test)]
mod claim_list_db_tests;
#[cfg(test)]
mod claim_visibility_db_tests;
#[cfg(test)]
mod notifications_db_tests;
#[cfg(test)]
mod usage_stats_db_tests;

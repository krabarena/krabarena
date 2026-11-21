use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::time::Duration as StdDuration;

use chrono::{DateTime, Utc};
use reqwest::header::RETRY_AFTER;
use serde::Serialize;
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

const DEFAULT_PUBLIC_BASE_URL: &str = "https://krabarena.com";
const DISCORD_API_BASE_URL: &str = "https://discord.com/api/v10";
pub const DISCORD_MONITOR_BATCH_LIMIT: i64 = 20;
pub const DISCORD_MONITOR_MESSAGE_LIMIT: usize = 1800;

#[derive(Clone, PartialEq, Eq)]
pub struct RedactedSecret(String);

impl RedactedSecret {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn is_configured(&self) -> bool {
        !self.0.trim().is_empty()
    }
}

impl fmt::Debug for RedactedSecret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("RedactedSecret(**redacted**)")
    }
}

#[derive(Clone)]
pub struct DiscordMonitorConfig {
    pub enabled: bool,
    pub bot_token: RedactedSecret,
    pub channel_id: String,
    pub public_base_url: String,
    pub cron_shared_secret: RedactedSecret,
}

impl DiscordMonitorConfig {
    pub fn from_env() -> Self {
        Self::from_lookup(|key| std::env::var(key).ok())
    }

    pub fn from_lookup(mut lookup: impl FnMut(&str) -> Option<String>) -> Self {
        let public_base_url =
            lookup("PUBLIC_BASE_URL").unwrap_or_else(|| DEFAULT_PUBLIC_BASE_URL.to_string());

        Self {
            enabled: lookup("DISCORD_MONITOR_ENABLED").as_deref() == Some("true"),
            bot_token: RedactedSecret::new(lookup("DISCORD_MONITOR_BOT_TOKEN").unwrap_or_default()),
            channel_id: lookup("DISCORD_MONITOR_CHANNEL_ID")
                .unwrap_or_default()
                .trim()
                .to_string(),
            public_base_url: normalize_public_base_url(&public_base_url),
            cron_shared_secret: RedactedSecret::new(
                lookup("CRON_SHARED_SECRET").unwrap_or_default(),
            ),
        }
    }

    pub fn has_bot_token(&self) -> bool {
        self.bot_token.is_configured()
    }

    pub fn has_channel_id(&self) -> bool {
        !self.channel_id.trim().is_empty()
    }

    pub fn can_send_to_discord(&self) -> bool {
        self.enabled && self.has_bot_token() && self.has_channel_id()
    }
}

impl fmt::Debug for DiscordMonitorConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DiscordMonitorConfig")
            .field("enabled", &self.enabled)
            .field("has_bot_token", &self.has_bot_token())
            .field("has_channel_id", &self.has_channel_id())
            .field("public_base_url", &self.public_base_url)
            .field(
                "has_cron_shared_secret",
                &self.cron_shared_secret.is_configured(),
            )
            .finish()
    }
}

fn normalize_public_base_url(value: &str) -> String {
    let trimmed = value.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        DEFAULT_PUBLIC_BASE_URL.to_string()
    } else {
        trimmed.to_string()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckpointKind {
    NewClaims,
    NewVerifications,
}

impl CheckpointKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NewClaims => "new_claims",
            Self::NewVerifications => "new_verifications",
        }
    }

    fn from_sql(value: &str) -> Option<Self> {
        match value {
            "new_claims" => Some(Self::NewClaims),
            "new_verifications" => Some(Self::NewVerifications),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscordMonitorCheckpoint {
    pub kind: CheckpointKind,
    pub last_created_at: Option<DateTime<Utc>>,
    pub last_id: Option<Uuid>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscordMonitorClaimAlert {
    pub claim_id: Uuid,
    pub first_alert_sent_at: Option<DateTime<Utc>>,
    pub last_alert_sent_at: Option<DateTime<Utc>>,
    pub alert_count: i32,
    pub last_alert_level: Option<String>,
    pub resolved_at: Option<DateTime<Utc>>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnverifiedAlertLevel {
    TwelveHours,
    TwentyFourHours,
    FortyEightPlus,
}

impl UnverifiedAlertLevel {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::TwelveHours => "12h",
            Self::TwentyFourHours => "24h",
            Self::FortyEightPlus => "48h+",
        }
    }

    fn from_sql(value: &str) -> Self {
        match value {
            "48h+" => Self::FortyEightPlus,
            "24h" => Self::TwentyFourHours,
            _ => Self::TwelveHours,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct DiscordMonitorClaimEvent {
    pub id: Uuid,
    pub slug: Option<String>,
    pub battle_id: Option<Uuid>,
    pub battle_slug: Option<String>,
    pub battle_title: Option<String>,
    pub claim: String,
    pub author_id: Uuid,
    pub author_display_name: String,
    pub author_email: Option<String>,
    pub visibility: String,
    pub agent_backend: Option<String>,
    pub agent_model: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct DiscordMonitorVerificationEvent {
    pub id: Uuid,
    pub claim_id: Uuid,
    pub claim_slug: Option<String>,
    pub battle_id: Option<Uuid>,
    pub battle_slug: Option<String>,
    pub battle_title: Option<String>,
    pub claim: String,
    pub verdict: String,
    pub author_id: Uuid,
    pub author_display_name: String,
    pub author_email: Option<String>,
    pub agent_backend: Option<String>,
    pub agent_model: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscordMonitorUnverifiedClaim {
    pub id: Uuid,
    pub slug: Option<String>,
    pub battle_id: Option<Uuid>,
    pub battle_slug: Option<String>,
    pub battle_title: Option<String>,
    pub claim: String,
    pub author_id: Uuid,
    pub author_display_name: String,
    pub author_email: Option<String>,
    pub visibility: String,
    pub agent_backend: Option<String>,
    pub agent_model: Option<String>,
    pub created_at: DateTime<Utc>,
    pub last_alert_sent_at: Option<DateTime<Utc>>,
    pub alert_count: i32,
    pub alert_level: UnverifiedAlertLevel,
}

#[derive(sqlx::FromRow)]
struct UnverifiedClaimRow {
    id: Uuid,
    slug: Option<String>,
    battle_id: Option<Uuid>,
    battle_slug: Option<String>,
    battle_title: Option<String>,
    claim: String,
    author_id: Uuid,
    author_display_name: String,
    author_email: Option<String>,
    visibility: String,
    agent_backend: Option<String>,
    agent_model: Option<String>,
    created_at: DateTime<Utc>,
    last_alert_sent_at: Option<DateTime<Utc>>,
    alert_count: i32,
    alert_level: String,
}

type CheckpointRow = (String, Option<DateTime<Utc>>, Option<Uuid>, DateTime<Utc>);

type ClaimAlertRow = (
    Uuid,
    Option<DateTime<Utc>>,
    Option<DateTime<Utc>>,
    i32,
    Option<String>,
    Option<DateTime<Utc>>,
    DateTime<Utc>,
);

impl From<UnverifiedClaimRow> for DiscordMonitorUnverifiedClaim {
    fn from(row: UnverifiedClaimRow) -> Self {
        Self {
            id: row.id,
            slug: row.slug,
            battle_id: row.battle_id,
            battle_slug: row.battle_slug,
            battle_title: row.battle_title,
            claim: row.claim,
            author_id: row.author_id,
            author_display_name: row.author_display_name,
            author_email: row.author_email,
            visibility: row.visibility,
            agent_backend: row.agent_backend,
            agent_model: row.agent_model,
            created_at: row.created_at,
            last_alert_sent_at: row.last_alert_sent_at,
            alert_count: row.alert_count,
            alert_level: UnverifiedAlertLevel::from_sql(&row.alert_level),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscordMonitorRunStatus {
    Success,
    Disabled,
    Failed,
}

impl DiscordMonitorRunStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Disabled => "disabled",
            Self::Failed => "failed",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscordMonitorRunRecord {
    pub id: Uuid,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub new_claims_count: i32,
    pub new_verifications_count: i32,
    pub unverified_claims_count: i32,
    pub messages_sent: i32,
    pub status: DiscordMonitorRunStatus,
    pub error_message: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DiscordMonitorRunCounters {
    pub new_claims_count: i32,
    pub new_verifications_count: i32,
    pub unverified_claims_count: i32,
    pub messages_sent: i32,
}

#[derive(Debug, Serialize)]
pub struct DiscordMonitorRunResponse {
    pub run_id: Uuid,
    pub status: &'static str,
    pub new_claims_count: i32,
    pub new_verifications_count: i32,
    pub unverified_claims_count: i32,
    pub messages_sent: i32,
}

impl DiscordMonitorRunResponse {
    pub fn from_record(record: &DiscordMonitorRunRecord) -> Self {
        Self {
            run_id: record.id,
            status: record.status.as_str(),
            new_claims_count: record.new_claims_count,
            new_verifications_count: record.new_verifications_count,
            unverified_claims_count: record.unverified_claims_count,
            messages_sent: record.messages_sent,
        }
    }

    pub fn failed(run_id: Uuid) -> Self {
        Self {
            run_id,
            status: DiscordMonitorRunStatus::Failed.as_str(),
            new_claims_count: 0,
            new_verifications_count: 0,
            unverified_claims_count: 0,
            messages_sent: 0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscordMonitorFormattedMessage {
    pub content: String,
    pub included_rows: usize,
    pub total_rows: usize,
}

impl DiscordMonitorFormattedMessage {
    fn new(content: String, included_rows: usize, total_rows: usize) -> Self {
        Self {
            content,
            included_rows,
            total_rows,
        }
    }
}

pub fn format_new_claims_message(
    public_base_url: &str,
    events: &[DiscordMonitorClaimEvent],
) -> Option<DiscordMonitorFormattedMessage> {
    if events.is_empty() {
        return None;
    }

    format_with_title_limit(events.len(), |title_limit| {
        let rows = events
            .iter()
            .take(DISCORD_MONITOR_BATCH_LIMIT as usize)
            .enumerate()
            .map(|(idx, event)| {
                format!(
                    "**{}. [{}]({})**\n> Battle: {}\n> Author: {} · 🕒 {}",
                    idx + 1,
                    claim_title(&event.claim, event.id, title_limit),
                    claim_url(public_base_url, event.slug.as_deref(), event.id),
                    battle_reference(
                        public_base_url,
                        event.battle_slug.as_deref(),
                        event.battle_id,
                        event.battle_title.as_deref(),
                        title_limit,
                    ),
                    clean_table_cell(&event.author_display_name, 40),
                    format_timestamp(event.created_at)
                )
            })
            .collect::<Vec<_>>();

        build_markdown_message(
            "## 🦀 New Claims",
            &format!("**Count:** {}", events.len()),
            &rows,
            events.len(),
        )
    })
}

pub fn format_new_verifications_message(
    public_base_url: &str,
    events: &[DiscordMonitorVerificationEvent],
) -> Option<DiscordMonitorFormattedMessage> {
    if events.is_empty() {
        return None;
    }

    format_with_title_limit(events.len(), |title_limit| {
        let rows = events
            .iter()
            .take(DISCORD_MONITOR_BATCH_LIMIT as usize)
            .enumerate()
            .map(|(idx, event)| {
                format!(
                    "**{}. {}: [{}]({})**\n> Battle: {}\n> Verifier: {} · 🕒 {}",
                    idx + 1,
                    verification_type(&event.verdict),
                    claim_title(&event.claim, event.claim_id, title_limit),
                    claim_url(public_base_url, event.claim_slug.as_deref(), event.claim_id),
                    battle_reference(
                        public_base_url,
                        event.battle_slug.as_deref(),
                        event.battle_id,
                        event.battle_title.as_deref(),
                        title_limit,
                    ),
                    clean_table_cell(&event.author_display_name, 40),
                    format_timestamp(event.created_at)
                )
            })
            .collect::<Vec<_>>();

        build_markdown_message(
            "## ✅ New Verifications",
            &format!("**Count:** {}", events.len()),
            &rows,
            events.len(),
        )
    })
}

pub fn format_unverified_claims_message(
    public_base_url: &str,
    now: DateTime<Utc>,
    claims: &[DiscordMonitorUnverifiedClaim],
) -> Option<DiscordMonitorFormattedMessage> {
    if claims.is_empty() {
        return None;
    }

    let twelve = claims
        .iter()
        .filter(|claim| claim.alert_level == UnverifiedAlertLevel::TwelveHours)
        .count();
    let twenty_four = claims
        .iter()
        .filter(|claim| claim.alert_level == UnverifiedAlertLevel::TwentyFourHours)
        .count();
    let forty_eight = claims
        .iter()
        .filter(|claim| claim.alert_level == UnverifiedAlertLevel::FortyEightPlus)
        .count();
    let count_line = format!(
        "Count: {} total - {} at 12h, {} at 24h, {} at 48h+",
        claims.len(),
        twelve,
        twenty_four,
        forty_eight
    );

    format_with_title_limit(claims.len(), |title_limit| {
        let rows = claims
            .iter()
            .take(DISCORD_MONITOR_BATCH_LIMIT as usize)
            .enumerate()
            .map(|(idx, claim)| {
                format!(
                    "**{}. [{}]({})**\n> Level: **{}**\n> Age: **{}**\n> Battle: {}\n> Author: {}",
                    idx + 1,
                    claim_title(&claim.claim, claim.id, title_limit),
                    claim_url(public_base_url, claim.slug.as_deref(), claim.id),
                    claim.alert_level.as_str(),
                    age_label(now, claim.created_at),
                    battle_reference(
                        public_base_url,
                        claim.battle_slug.as_deref(),
                        claim.battle_id,
                        claim.battle_title.as_deref(),
                        title_limit,
                    ),
                    clean_table_cell(&claim.author_display_name, 40)
                )
            })
            .collect::<Vec<_>>();

        build_markdown_message(
            "## ⏳ Claims without verification for 12h+",
            &format!("**{count_line}**"),
            &rows,
            claims.len(),
        )
    })
}

fn format_with_title_limit(
    total_rows: usize,
    build: impl Fn(usize) -> Option<DiscordMonitorFormattedMessage>,
) -> Option<DiscordMonitorFormattedMessage> {
    for title_limit in [90, 64, 40, 24, 12] {
        if let Some(message) = build(title_limit)
            && (message.included_rows > 0 || total_rows == 0)
        {
            return Some(message);
        }
    }
    None
}

fn build_markdown_message(
    title: &str,
    count_line: &str,
    rows: &[String],
    total_rows: usize,
) -> Option<DiscordMonitorFormattedMessage> {
    if rows.is_empty() {
        return None;
    }

    let prefix = format!("{title}\n{count_line}\n\n");
    let mut included = 0usize;
    let mut body = String::new();

    for row in rows {
        let next_body = if body.is_empty() {
            row.to_string()
        } else {
            format!("{body}\n\n{row}")
        };
        let next_included = included + 1;
        let omitted = total_rows.saturating_sub(next_included);
        let mut candidate = format!("{prefix}{next_body}");
        if omitted > 0 {
            candidate.push_str(&format!("\n\n…and {omitted} more"));
        }
        if candidate.len() > DISCORD_MONITOR_MESSAGE_LIMIT {
            break;
        }
        body = next_body;
        included = next_included;
    }

    if included == 0 {
        return Some(DiscordMonitorFormattedMessage::new(
            format!("{prefix}…and {total_rows} more"),
            0,
            total_rows,
        ));
    }

    let omitted = total_rows.saturating_sub(included);
    let mut content = format!("{prefix}{body}");
    if omitted > 0 {
        content.push_str(&format!("\n\n…and {omitted} more"));
    }

    Some(DiscordMonitorFormattedMessage::new(
        content, included, total_rows,
    ))
}

fn claim_url(public_base_url: &str, slug: Option<&str>, claim_id: Uuid) -> String {
    let base = normalize_public_base_url(public_base_url);
    let identifier = slug
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| claim_id.to_string());
    format!("{}/claims/{}", base, identifier)
}

fn battle_url(public_base_url: &str, slug: Option<&str>, battle_id: Uuid) -> String {
    let base = normalize_public_base_url(public_base_url);
    let identifier = slug
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| battle_id.to_string());
    format!("{}/battles/{}", base, identifier)
}

fn claim_title(claim: &str, claim_id: Uuid, max_chars: usize) -> String {
    let title = claim
        .lines()
        .find_map(|line| {
            let trimmed = line.trim();
            trimmed
                .strip_prefix("# ")
                .map(str::trim)
                .filter(|value| !value.is_empty())
        })
        .or_else(|| claim.lines().map(str::trim).find(|line| !line.is_empty()))
        .map(str::to_string)
        .unwrap_or_else(|| format!("claim {}", short_id(claim_id)));

    clean_table_cell(&title, max_chars)
}

fn battle_label(title: Option<&str>, max_chars: usize) -> String {
    title
        .map(|value| clean_table_cell(value, max_chars))
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "Standalone".to_string())
}

fn battle_reference(
    public_base_url: &str,
    slug: Option<&str>,
    battle_id: Option<Uuid>,
    title: Option<&str>,
    max_chars: usize,
) -> String {
    let label = battle_label(title, max_chars);
    match battle_id {
        Some(id) => format!("[{}]({})", label, battle_url(public_base_url, slug, id)),
        None => label,
    }
}

fn clean_table_cell(value: &str, max_chars: usize) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        let safe = match ch {
            '\n' | '\r' | '\t' => ' ',
            '|' | '`' | '*' | '_' | '[' | ']' | '(' | ')' | '<' | '>' | '#' | '~' => ' ',
            _ => ch,
        };
        out.push(safe);
    }
    let collapsed = out.split_whitespace().collect::<Vec<_>>().join(" ");
    truncate_chars(&collapsed, max_chars)
}

fn truncate_chars(value: &str, max_chars: usize) -> String {
    let char_count = value.chars().count();
    if char_count <= max_chars {
        return value.to_string();
    }
    if max_chars <= 3 {
        return value.chars().take(max_chars).collect();
    }
    let mut out = value.chars().take(max_chars - 3).collect::<String>();
    out.push_str("...");
    out
}

fn short_id(id: Uuid) -> String {
    id.to_string().chars().take(8).collect()
}

fn verification_type(verdict: &str) -> &'static str {
    match verdict {
        "confirmed" => "Verify",
        "disconfirmed" => "Refute",
        _ => "Unknown",
    }
}

fn format_timestamp(ts: DateTime<Utc>) -> String {
    ts.format("%Y-%m-%d %H:%M UTC").to_string()
}

fn age_label(now: DateTime<Utc>, created_at: DateTime<Utc>) -> String {
    let hours = (now - created_at).num_hours().max(0);
    format!("{hours}h")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscordSendErrorKind {
    PayloadValidation,
    BotAuthorization,
    BotPermissions,
    ChannelNotFound,
    RateLimited,
    DiscordUnavailable,
    NetworkError,
    Timeout,
}

impl DiscordSendErrorKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PayloadValidation => "payload_validation",
            Self::BotAuthorization => "bot_authorization",
            Self::BotPermissions => "bot_permissions",
            Self::ChannelNotFound => "channel_not_found",
            Self::RateLimited => "rate_limited",
            Self::DiscordUnavailable => "discord_unavailable",
            Self::NetworkError => "network_error",
            Self::Timeout => "timeout",
        }
    }

    fn is_retryable(self) -> bool {
        matches!(
            self,
            Self::RateLimited | Self::DiscordUnavailable | Self::NetworkError | Self::Timeout
        )
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct DiscordSendError {
    pub kind: DiscordSendErrorKind,
    pub status: Option<u16>,
    pub retry_after: Option<StdDuration>,
    pub attempts: usize,
}

impl DiscordSendError {
    fn new(
        kind: DiscordSendErrorKind,
        status: Option<u16>,
        retry_after: Option<StdDuration>,
        attempts: usize,
    ) -> Self {
        Self {
            kind,
            status,
            retry_after,
            attempts,
        }
    }

    pub fn sanitized_message(&self) -> String {
        match (self.status, self.retry_after) {
            (Some(status), Some(retry_after)) => format!(
                "discord send failed: kind={}, status={}, attempts={}, retry_after_ms={}",
                self.kind.as_str(),
                status,
                self.attempts,
                retry_after.as_millis()
            ),
            (Some(status), None) => format!(
                "discord send failed: kind={}, status={}, attempts={}",
                self.kind.as_str(),
                status,
                self.attempts
            ),
            (None, _) => format!(
                "discord send failed: kind={}, attempts={}",
                self.kind.as_str(),
                self.attempts
            ),
        }
    }
}

impl fmt::Debug for DiscordSendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DiscordSendError")
            .field("kind", &self.kind)
            .field("status", &self.status)
            .field("retry_after", &self.retry_after)
            .field("attempts", &self.attempts)
            .finish()
    }
}

impl fmt::Display for DiscordSendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.sanitized_message())
    }
}

impl std::error::Error for DiscordSendError {}

#[derive(Debug, Clone)]
pub struct DiscordRetryPolicy {
    retry_backoffs: [StdDuration; 2],
    max_rate_limit_delay: StdDuration,
}

impl Default for DiscordRetryPolicy {
    fn default() -> Self {
        Self {
            retry_backoffs: [StdDuration::from_secs(1), StdDuration::from_secs(3)],
            max_rate_limit_delay: StdDuration::from_secs(15),
        }
    }
}

impl DiscordRetryPolicy {
    #[cfg(test)]
    fn zero_delay() -> Self {
        Self {
            retry_backoffs: [StdDuration::ZERO, StdDuration::ZERO],
            max_rate_limit_delay: StdDuration::ZERO,
        }
    }

    fn delay_for(
        &self,
        kind: DiscordSendErrorKind,
        retry_after: Option<StdDuration>,
        attempt: usize,
    ) -> StdDuration {
        if kind == DiscordSendErrorKind::RateLimited {
            return retry_after
                .unwrap_or_else(|| StdDuration::from_secs(1))
                .min(self.max_rate_limit_delay);
        }
        self.retry_backoffs
            .get(attempt.saturating_sub(1))
            .copied()
            .unwrap_or_else(|| StdDuration::from_secs(3))
    }
}

#[derive(Clone)]
pub struct DiscordBotClient {
    http: reqwest::Client,
    base_url: String,
    channel_id: String,
    bot_token: RedactedSecret,
    retry_policy: DiscordRetryPolicy,
}

impl DiscordBotClient {
    pub fn from_config(config: &DiscordMonitorConfig) -> Self {
        Self::new(
            config.bot_token.clone(),
            config.channel_id.clone(),
            DISCORD_API_BASE_URL.to_string(),
            DiscordRetryPolicy::default(),
        )
    }

    fn new(
        bot_token: RedactedSecret,
        channel_id: String,
        base_url: String,
        retry_policy: DiscordRetryPolicy,
    ) -> Self {
        Self::new_with_timeout(
            bot_token,
            channel_id,
            base_url,
            retry_policy,
            StdDuration::from_secs(10),
        )
    }

    fn new_with_timeout(
        bot_token: RedactedSecret,
        channel_id: String,
        base_url: String,
        retry_policy: DiscordRetryPolicy,
        timeout: StdDuration,
    ) -> Self {
        let http = reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self {
            http,
            base_url: base_url.trim_end_matches('/').to_string(),
            channel_id,
            bot_token,
            retry_policy,
        }
    }

    #[cfg(test)]
    fn with_base_url(bot_token: &str, channel_id: &str, base_url: &str) -> Self {
        Self::new(
            RedactedSecret::new(bot_token),
            channel_id.to_string(),
            base_url.to_string(),
            DiscordRetryPolicy::zero_delay(),
        )
    }

    #[cfg(test)]
    fn with_base_url_and_timeout(
        bot_token: &str,
        channel_id: &str,
        base_url: &str,
        timeout: StdDuration,
    ) -> Self {
        Self::new_with_timeout(
            RedactedSecret::new(bot_token),
            channel_id.to_string(),
            base_url.to_string(),
            DiscordRetryPolicy::zero_delay(),
            timeout,
        )
    }

    pub async fn send_message(&self, content: &str) -> Result<(), DiscordSendError> {
        if content.len() > DISCORD_MONITOR_MESSAGE_LIMIT {
            return Err(DiscordSendError::new(
                DiscordSendErrorKind::PayloadValidation,
                None,
                None,
                0,
            ));
        }

        let url = format!("{}/channels/{}/messages", self.base_url, self.channel_id);
        let body = json!({ "content": content });
        let mut attempts = 0usize;

        loop {
            attempts += 1;
            let response = self
                .http
                .post(&url)
                .header("Authorization", format!("Bot {}", self.bot_token.as_str()))
                .json(&body)
                .send()
                .await;

            let error = match response {
                Ok(resp) if resp.status().is_success() => return Ok(()),
                Ok(resp) => {
                    let status = resp.status();
                    let retry_after_header = parse_retry_after_header(&resp);
                    let text = resp.text().await.unwrap_or_default();
                    let retry_after = retry_after_header.or_else(|| parse_retry_after_json(&text));
                    let kind = classify_status(status.as_u16());
                    DiscordSendError::new(kind, Some(status.as_u16()), retry_after, attempts)
                }
                Err(err) if err.is_timeout() => {
                    DiscordSendError::new(DiscordSendErrorKind::Timeout, None, None, attempts)
                }
                Err(_) => {
                    DiscordSendError::new(DiscordSendErrorKind::NetworkError, None, None, attempts)
                }
            };

            if attempts >= 3 || !error.kind.is_retryable() {
                return Err(error);
            }

            let delay = self
                .retry_policy
                .delay_for(error.kind, error.retry_after, attempts);
            if !delay.is_zero() {
                tokio::time::sleep(delay).await;
            }
        }
    }
}

pub trait DiscordMessageSender {
    fn send_discord_message<'a>(
        &'a self,
        content: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<(), DiscordSendError>> + Send + 'a>>;
}

impl DiscordMessageSender for DiscordBotClient {
    fn send_discord_message<'a>(
        &'a self,
        content: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<(), DiscordSendError>> + Send + 'a>> {
        Box::pin(async move { self.send_message(content).await })
    }
}

fn classify_status(status: u16) -> DiscordSendErrorKind {
    match status {
        400 => DiscordSendErrorKind::PayloadValidation,
        401 => DiscordSendErrorKind::BotAuthorization,
        403 => DiscordSendErrorKind::BotPermissions,
        404 => DiscordSendErrorKind::ChannelNotFound,
        429 => DiscordSendErrorKind::RateLimited,
        500..=599 => DiscordSendErrorKind::DiscordUnavailable,
        _ => DiscordSendErrorKind::PayloadValidation,
    }
}

fn parse_retry_after_header(resp: &reqwest::Response) -> Option<StdDuration> {
    resp.headers()
        .get(RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(parse_retry_after_seconds)
}

fn parse_retry_after_json(text: &str) -> Option<StdDuration> {
    let value: serde_json::Value = serde_json::from_str(text).ok()?;
    let seconds = value.get("retry_after")?.as_f64()?;
    duration_from_seconds(seconds)
}

fn parse_retry_after_seconds(value: &str) -> Option<StdDuration> {
    let seconds = value.trim().parse::<f64>().ok()?;
    duration_from_seconds(seconds)
}

fn duration_from_seconds(seconds: f64) -> Option<StdDuration> {
    if !seconds.is_finite() || seconds < 0.0 {
        return None;
    }
    Some(StdDuration::from_millis((seconds * 1000.0).round() as u64))
}

pub fn is_authorized_cron_request(
    config: &DiscordMonitorConfig,
    provided_secret: Option<&str>,
) -> bool {
    // Constant-time compare: a byte-by-byte `==` leaks how many leading
    // bytes of the cron secret matched via response timing.
    config.cron_shared_secret.is_configured()
        && provided_secret
            .is_some_and(|p| crate::web::cookies::ct_eq(p, config.cron_shared_secret.as_str()))
}

pub fn determine_run_status(config: &DiscordMonitorConfig) -> DiscordMonitorRunStatus {
    if config.can_send_to_discord() {
        DiscordMonitorRunStatus::Success
    } else {
        DiscordMonitorRunStatus::Disabled
    }
}

pub async fn load_checkpoint(
    pool: &PgPool,
    kind: CheckpointKind,
) -> Result<Option<DiscordMonitorCheckpoint>, sqlx::Error> {
    let row: Option<CheckpointRow> = sqlx::query_as(
        r#"
        SELECT kind, last_created_at, last_id, updated_at
        FROM discord_monitor_checkpoints
        WHERE kind = $1
        "#,
    )
    .bind(kind.as_str())
    .fetch_optional(pool)
    .await?;

    Ok(
        row.and_then(|(kind, last_created_at, last_id, updated_at)| {
            CheckpointKind::from_sql(&kind).map(|kind| DiscordMonitorCheckpoint {
                kind,
                last_created_at,
                last_id,
                updated_at,
            })
        }),
    )
}

pub async fn save_checkpoint_after_sent(
    pool: &PgPool,
    kind: CheckpointKind,
    last_created_at: DateTime<Utc>,
    last_id: Uuid,
) -> Result<DiscordMonitorCheckpoint, sqlx::Error> {
    let (kind, last_created_at, last_id, updated_at): (
        String,
        Option<DateTime<Utc>>,
        Option<Uuid>,
        DateTime<Utc>,
    ) = sqlx::query_as(
        r#"
        INSERT INTO discord_monitor_checkpoints (kind, last_created_at, last_id, updated_at)
        VALUES ($1, $2, $3, NOW())
        ON CONFLICT (kind) DO UPDATE
        SET last_created_at = EXCLUDED.last_created_at,
            last_id = EXCLUDED.last_id,
            updated_at = NOW()
        RETURNING kind, last_created_at, last_id, updated_at
        "#,
    )
    .bind(kind.as_str())
    .bind(last_created_at)
    .bind(last_id)
    .fetch_one(pool)
    .await?;

    Ok(DiscordMonitorCheckpoint {
        kind: CheckpointKind::from_sql(&kind).unwrap_or(CheckpointKind::NewClaims),
        last_created_at,
        last_id,
        updated_at,
    })
}

pub async fn fetch_new_claim_events(
    pool: &PgPool,
) -> Result<Vec<DiscordMonitorClaimEvent>, sqlx::Error> {
    let checkpoint = load_checkpoint(pool, CheckpointKind::NewClaims).await?;
    fetch_new_claim_events_after(pool, checkpoint.as_ref()).await
}

pub async fn fetch_new_claim_events_after(
    pool: &PgPool,
    checkpoint: Option<&DiscordMonitorCheckpoint>,
) -> Result<Vec<DiscordMonitorClaimEvent>, sqlx::Error> {
    let superseded = crate::models::claim::not_superseded("c");
    let sql = format!(
        r#"
        SELECT
            c.id,
            c.slug,
            c.battle_id,
            b.slug AS battle_slug,
            b.title AS battle_title,
            c.claim,
            c.author_id,
            u.display_name AS author_display_name,
            u.email AS author_email,
            COALESCE(c.visibility, 'public') AS visibility,
            c.agent_backend,
            c.agent_model,
            c.created_at
        FROM claims c
        JOIN users u ON u.id = c.author_id
        LEFT JOIN battles b ON b.id = c.battle_id
        WHERE c.deleted_at IS NULL
          AND (c.battle_id IS NULL OR b.deleted_at IS NULL)
          {superseded}
          AND (
              $1::timestamptz IS NULL
              OR $2::uuid IS NULL
              OR (c.created_at, c.id) > ($1::timestamptz, $2::uuid)
          )
        ORDER BY c.created_at ASC, c.id ASC
        LIMIT $3
        "#,
    );

    sqlx::query_as::<_, DiscordMonitorClaimEvent>(&sql)
        .bind(checkpoint.and_then(|c| c.last_created_at))
        .bind(checkpoint.and_then(|c| c.last_id))
        .bind(DISCORD_MONITOR_BATCH_LIMIT)
        .fetch_all(pool)
        .await
}

pub async fn fetch_new_verification_events(
    pool: &PgPool,
) -> Result<Vec<DiscordMonitorVerificationEvent>, sqlx::Error> {
    let checkpoint = load_checkpoint(pool, CheckpointKind::NewVerifications).await?;
    fetch_new_verification_events_after(pool, checkpoint.as_ref()).await
}

pub async fn fetch_new_verification_events_after(
    pool: &PgPool,
    checkpoint: Option<&DiscordMonitorCheckpoint>,
) -> Result<Vec<DiscordMonitorVerificationEvent>, sqlx::Error> {
    let superseded = crate::models::claim::not_superseded("c");
    let sql = format!(
        r#"
        SELECT
            v.id,
            v.claim_id,
            c.slug AS claim_slug,
            c.battle_id,
            b.slug AS battle_slug,
            b.title AS battle_title,
            c.claim,
            v.verdict,
            v.author_id,
            u.display_name AS author_display_name,
            u.email AS author_email,
            v.agent_backend,
            v.agent_model,
            v.created_at
        FROM verifications v
        JOIN claims c ON c.id = v.claim_id
        JOIN users u ON u.id = v.author_id
        LEFT JOIN battles b ON b.id = c.battle_id
        WHERE c.deleted_at IS NULL
          AND (c.battle_id IS NULL OR b.deleted_at IS NULL)
          {superseded}
          AND (
              $1::timestamptz IS NULL
              OR $2::uuid IS NULL
              OR (v.created_at, v.id) > ($1::timestamptz, $2::uuid)
          )
        ORDER BY v.created_at ASC, v.id ASC
        LIMIT $3
        "#,
    );

    sqlx::query_as::<_, DiscordMonitorVerificationEvent>(&sql)
        .bind(checkpoint.and_then(|c| c.last_created_at))
        .bind(checkpoint.and_then(|c| c.last_id))
        .bind(DISCORD_MONITOR_BATCH_LIMIT)
        .fetch_all(pool)
        .await
}

pub async fn fetch_unverified_claims(
    pool: &PgPool,
    now: DateTime<Utc>,
) -> Result<Vec<DiscordMonitorUnverifiedClaim>, sqlx::Error> {
    let superseded = crate::models::claim::not_superseded("c");
    let sql = format!(
        r#"
        SELECT
            c.id,
            c.slug,
            c.battle_id,
            b.slug AS battle_slug,
            b.title AS battle_title,
            c.claim,
            c.author_id,
            u.display_name AS author_display_name,
            u.email AS author_email,
            COALESCE(c.visibility, 'public') AS visibility,
            c.agent_backend,
            c.agent_model,
            c.created_at,
            a.last_alert_sent_at,
            COALESCE(a.alert_count, 0) AS alert_count,
            CASE
                WHEN c.created_at <= $1::timestamptz - INTERVAL '48 hours' THEN '48h+'
                WHEN c.created_at <= $1::timestamptz - INTERVAL '24 hours' THEN '24h'
                ELSE '12h'
            END AS alert_level
        FROM claims c
        JOIN users u ON u.id = c.author_id
        LEFT JOIN battles b ON b.id = c.battle_id
        LEFT JOIN discord_monitor_claim_alerts a ON a.claim_id = c.id
        WHERE c.deleted_at IS NULL
          AND (c.battle_id IS NULL OR b.deleted_at IS NULL)
          {superseded}
          AND c.created_at <= $1::timestamptz - INTERVAL '12 hours'
          AND NOT EXISTS (SELECT 1 FROM verifications v WHERE v.claim_id = c.id)
          AND (
              a.claim_id IS NULL
              OR (
                  a.resolved_at IS NULL
                  AND (
                      a.last_alert_sent_at IS NULL
                      OR a.last_alert_sent_at <= $1::timestamptz - INTERVAL '24 hours'
                  )
              )
          )
        ORDER BY c.created_at ASC, c.id ASC
        LIMIT $2
        "#,
    );

    let rows = sqlx::query_as::<_, UnverifiedClaimRow>(&sql)
        .bind(now)
        .bind(DISCORD_MONITOR_BATCH_LIMIT)
        .fetch_all(pool)
        .await?;

    Ok(rows.into_iter().map(Into::into).collect())
}

pub async fn record_unverified_alert_sent(
    pool: &PgPool,
    claim_id: Uuid,
    level: UnverifiedAlertLevel,
    sent_at: DateTime<Utc>,
) -> Result<DiscordMonitorClaimAlert, sqlx::Error> {
    let (
        claim_id,
        first_alert_sent_at,
        last_alert_sent_at,
        alert_count,
        last_alert_level,
        resolved_at,
        updated_at,
    ): ClaimAlertRow = sqlx::query_as(
        r#"
        INSERT INTO discord_monitor_claim_alerts (
            claim_id,
            first_alert_sent_at,
            last_alert_sent_at,
            alert_count,
            last_alert_level,
            updated_at
        )
        VALUES ($1, $2, $2, 1, $3, $2)
        ON CONFLICT (claim_id) DO UPDATE
        SET first_alert_sent_at = COALESCE(
                discord_monitor_claim_alerts.first_alert_sent_at,
                EXCLUDED.first_alert_sent_at
            ),
            last_alert_sent_at = EXCLUDED.last_alert_sent_at,
            alert_count = discord_monitor_claim_alerts.alert_count + 1,
            last_alert_level = EXCLUDED.last_alert_level,
            updated_at = EXCLUDED.updated_at
        RETURNING
            claim_id,
            first_alert_sent_at,
            last_alert_sent_at,
            alert_count,
            last_alert_level,
            resolved_at,
            updated_at
        "#,
    )
    .bind(claim_id)
    .bind(sent_at)
    .bind(level.as_str())
    .fetch_one(pool)
    .await?;

    Ok(DiscordMonitorClaimAlert {
        claim_id,
        first_alert_sent_at,
        last_alert_sent_at,
        alert_count,
        last_alert_level,
        resolved_at,
        updated_at,
    })
}

pub async fn resolve_completed_claim_alerts(
    pool: &PgPool,
    resolved_at: DateTime<Utc>,
) -> Result<u64, sqlx::Error> {
    let result = sqlx::query(
        r#"
        UPDATE discord_monitor_claim_alerts a
        SET resolved_at = $1,
            updated_at = $1
        WHERE a.resolved_at IS NULL
          AND EXISTS (
              SELECT 1
              FROM claims c
              WHERE c.id = a.claim_id
                AND (
                    c.deleted_at IS NOT NULL
                    OR EXISTS (SELECT 1 FROM verifications v WHERE v.claim_id = c.id)
                    OR EXISTS (
                        SELECT 1
                        FROM claims sup
                        WHERE sup.predecessor_id = c.id
                          AND sup.deleted_at IS NULL
                    )
                )
          )
        "#,
    )
    .bind(resolved_at)
    .execute(pool)
    .await?;

    Ok(result.rows_affected())
}

#[derive(Debug)]
pub struct DiscordMonitorRunError {
    pub counters: DiscordMonitorRunCounters,
    message: String,
}

impl DiscordMonitorRunError {
    fn database(counters: DiscordMonitorRunCounters, error: sqlx::Error) -> Self {
        tracing::error!(
            error = %error,
            "discord.monitor.database_error"
        );
        Self {
            counters,
            message: "discord monitor database error".to_string(),
        }
    }

    fn discord(counters: DiscordMonitorRunCounters, error: DiscordSendError) -> Self {
        tracing::warn!(
            error = %error,
            "discord.monitor.send_error"
        );
        Self {
            counters,
            message: error.sanitized_message(),
        }
    }

    pub fn sanitized_message(&self) -> &str {
        &self.message
    }
}

pub async fn run_enabled_monitoring<S: DiscordMessageSender + Sync>(
    pool: &PgPool,
    config: &DiscordMonitorConfig,
    sender: &S,
    now: DateTime<Utc>,
) -> Result<DiscordMonitorRunCounters, DiscordMonitorRunError> {
    let mut counters = DiscordMonitorRunCounters::default();

    resolve_completed_claim_alerts(pool, now)
        .await
        .map_err(|error| DiscordMonitorRunError::database(counters, error))?;

    let new_claims = fetch_new_claim_events(pool)
        .await
        .map_err(|error| DiscordMonitorRunError::database(counters, error))?;
    if let Some(message) = format_new_claims_message(&config.public_base_url, &new_claims)
        && message.included_rows > 0
    {
        sender
            .send_discord_message(&message.content)
            .await
            .map_err(|error| DiscordMonitorRunError::discord(counters, error))?;
        let last = &new_claims[message.included_rows - 1];
        save_checkpoint_after_sent(pool, CheckpointKind::NewClaims, last.created_at, last.id)
            .await
            .map_err(|error| DiscordMonitorRunError::database(counters, error))?;
        counters.new_claims_count = message.included_rows as i32;
        counters.messages_sent += 1;
    }

    let new_verifications = fetch_new_verification_events(pool)
        .await
        .map_err(|error| DiscordMonitorRunError::database(counters, error))?;
    if let Some(message) =
        format_new_verifications_message(&config.public_base_url, &new_verifications)
        && message.included_rows > 0
    {
        sender
            .send_discord_message(&message.content)
            .await
            .map_err(|error| DiscordMonitorRunError::discord(counters, error))?;
        let last = &new_verifications[message.included_rows - 1];
        save_checkpoint_after_sent(
            pool,
            CheckpointKind::NewVerifications,
            last.created_at,
            last.id,
        )
        .await
        .map_err(|error| DiscordMonitorRunError::database(counters, error))?;
        counters.new_verifications_count = message.included_rows as i32;
        counters.messages_sent += 1;
    }

    let unverified = fetch_unverified_claims(pool, now)
        .await
        .map_err(|error| DiscordMonitorRunError::database(counters, error))?;
    if let Some(message) =
        format_unverified_claims_message(&config.public_base_url, now, &unverified)
        && message.included_rows > 0
    {
        sender
            .send_discord_message(&message.content)
            .await
            .map_err(|error| DiscordMonitorRunError::discord(counters, error))?;
        for claim in unverified.iter().take(message.included_rows) {
            record_unverified_alert_sent(pool, claim.id, claim.alert_level, now)
                .await
                .map_err(|error| DiscordMonitorRunError::database(counters, error))?;
        }
        counters.unverified_claims_count = message.included_rows as i32;
        counters.messages_sent += 1;
    }

    Ok(counters)
}

pub async fn record_monitor_run(
    pool: &PgPool,
    run_id: Uuid,
    started_at: DateTime<Utc>,
    finished_at: DateTime<Utc>,
    status: DiscordMonitorRunStatus,
    counters: DiscordMonitorRunCounters,
    error_message: Option<&str>,
) -> Result<DiscordMonitorRunRecord, sqlx::Error> {
    sqlx::query(
        r#"
        INSERT INTO discord_monitor_runs (
            id,
            started_at,
            finished_at,
            new_claims_count,
            new_verifications_count,
            unverified_claims_count,
            messages_sent,
            status,
            error_message
        )
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
        "#,
    )
    .bind(run_id)
    .bind(started_at)
    .bind(finished_at)
    .bind(counters.new_claims_count)
    .bind(counters.new_verifications_count)
    .bind(counters.unverified_claims_count)
    .bind(counters.messages_sent)
    .bind(status.as_str())
    .bind(error_message)
    .execute(pool)
    .await?;

    Ok(DiscordMonitorRunRecord {
        id: run_id,
        started_at,
        finished_at: Some(finished_at),
        new_claims_count: counters.new_claims_count,
        new_verifications_count: counters.new_verifications_count,
        unverified_claims_count: counters.unverified_claims_count,
        messages_sent: counters.messages_sent,
        status,
        error_message: error_message.map(str::to_string),
    })
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, VecDeque};
    use std::sync::{Arc, OnceLock};

    use super::*;
    use axum::body::Body;
    use axum::extract::{Path, State};
    use axum::http::{HeaderMap, HeaderValue, StatusCode};
    use axum::response::{IntoResponse, Response};
    use axum::routing::post;
    use axum::{Json, Router};
    use chrono::Duration;
    use serde_json::Value;
    use sqlx::postgres::PgPoolOptions;
    use tokio::net::TcpListener;

    const MIGRATION_SQL: &str = include_str!("../db/migrations/066_discord_claim_monitoring.sql");

    fn config_from(values: &[(&str, &str)]) -> DiscordMonitorConfig {
        let map = values
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect::<HashMap<_, _>>();
        DiscordMonitorConfig::from_lookup(|key| map.get(key).cloned())
    }

    fn sample_claim_event(id: Uuid, slug: Option<&str>, claim: &str) -> DiscordMonitorClaimEvent {
        DiscordMonitorClaimEvent {
            id,
            slug: slug.map(str::to_string),
            battle_id: Some(Uuid::new_v4()),
            battle_slug: Some("battle-slug".into()),
            battle_title: Some("Battle **Title**".into()),
            claim: claim.into(),
            author_id: Uuid::new_v4(),
            author_display_name: "Author_Name".into(),
            author_email: Some("author@example.test".into()),
            visibility: "private".into(),
            agent_backend: Some("codex".into()),
            agent_model: Some("gpt-5".into()),
            created_at: Utc::now(),
        }
    }

    fn sample_verification_event(
        id: Uuid,
        claim_id: Uuid,
        verdict: &str,
    ) -> DiscordMonitorVerificationEvent {
        DiscordMonitorVerificationEvent {
            id,
            claim_id,
            claim_slug: Some("verified-claim".into()),
            battle_id: Some(Uuid::new_v4()),
            battle_slug: Some("battle-slug".into()),
            battle_title: Some("Verification Battle".into()),
            claim: "# Verified Claim\n\nFull private markdown should not appear.".into(),
            verdict: verdict.into(),
            author_id: Uuid::new_v4(),
            author_display_name: "Verifier".into(),
            author_email: None,
            agent_backend: None,
            agent_model: None,
            created_at: Utc::now(),
        }
    }

    fn sample_unverified_claim(
        id: Uuid,
        level: UnverifiedAlertLevel,
        created_at: DateTime<Utc>,
    ) -> DiscordMonitorUnverifiedClaim {
        DiscordMonitorUnverifiedClaim {
            id,
            slug: Some(format!("unverified-{id}")),
            battle_id: None,
            battle_slug: None,
            battle_title: None,
            claim: "# Unverified Claim\n\n## Summary\nPrivate body should not be included.".into(),
            author_id: Uuid::new_v4(),
            author_display_name: "Unverified Author".into(),
            author_email: None,
            visibility: "private".into(),
            agent_backend: None,
            agent_model: None,
            created_at,
            last_alert_sent_at: None,
            alert_count: 0,
            alert_level: level,
        }
    }

    #[derive(Clone)]
    struct MockDiscordState {
        responses: Arc<tokio::sync::Mutex<VecDeque<MockDiscordResponse>>>,
        requests: Arc<tokio::sync::Mutex<Vec<CapturedDiscordRequest>>>,
    }

    #[derive(Clone)]
    struct MockDiscordResponse {
        status: StatusCode,
        retry_after: Option<&'static str>,
        body: &'static str,
        delay: Option<StdDuration>,
    }

    impl MockDiscordResponse {
        fn new(status: StatusCode, retry_after: Option<&'static str>, body: &'static str) -> Self {
            Self {
                status,
                retry_after,
                body,
                delay: None,
            }
        }

        fn delayed(status: StatusCode, delay: StdDuration) -> Self {
            Self {
                status,
                retry_after: None,
                body: "{}",
                delay: Some(delay),
            }
        }
    }

    #[derive(Debug, Clone)]
    struct CapturedDiscordRequest {
        channel_id: String,
        authorization: Option<String>,
        body: Value,
    }

    async fn spawn_discord_mock(
        responses: Vec<MockDiscordResponse>,
    ) -> (String, Arc<tokio::sync::Mutex<Vec<CapturedDiscordRequest>>>) {
        let state = MockDiscordState {
            responses: Arc::new(tokio::sync::Mutex::new(VecDeque::from(responses))),
            requests: Arc::new(tokio::sync::Mutex::new(Vec::new())),
        };
        let requests = state.requests.clone();
        let app = Router::new()
            .route(
                "/api/v10/channels/{channel_id}/messages",
                post(mock_discord_handler),
            )
            .with_state(state);
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock discord");
        let addr = listener.local_addr().expect("mock discord addr");
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        (format!("http://{addr}/api/v10"), requests)
    }

    async fn mock_discord_handler(
        Path(channel_id): Path<String>,
        State(state): State<MockDiscordState>,
        headers: HeaderMap,
        Json(body): Json<Value>,
    ) -> Response {
        state.requests.lock().await.push(CapturedDiscordRequest {
            channel_id,
            authorization: headers
                .get("Authorization")
                .and_then(|value| value.to_str().ok())
                .map(str::to_string),
            body,
        });

        let response = state
            .responses
            .lock()
            .await
            .pop_front()
            .unwrap_or(MockDiscordResponse {
                status: StatusCode::OK,
                retry_after: None,
                body: "{}",
                delay: None,
            });
        if let Some(delay) = response.delay {
            tokio::time::sleep(delay).await;
        }

        let mut builder = Response::builder().status(response.status);
        if let Some(retry_after) = response.retry_after {
            builder = builder.header("Retry-After", HeaderValue::from_static(retry_after));
        }
        builder
            .header("Content-Type", "application/json")
            .body(Body::from(response.body))
            .expect("mock discord response")
            .into_response()
    }

    #[derive(Clone)]
    struct TestSender {
        sent: Arc<tokio::sync::Mutex<Vec<String>>>,
        results: Arc<tokio::sync::Mutex<VecDeque<Result<(), DiscordSendError>>>>,
    }

    impl TestSender {
        fn success() -> Self {
            Self {
                sent: Arc::new(tokio::sync::Mutex::new(Vec::new())),
                results: Arc::new(tokio::sync::Mutex::new(VecDeque::new())),
            }
        }

        fn with_results(results: Vec<Result<(), DiscordSendError>>) -> Self {
            Self {
                sent: Arc::new(tokio::sync::Mutex::new(Vec::new())),
                results: Arc::new(tokio::sync::Mutex::new(VecDeque::from(results))),
            }
        }
    }

    impl DiscordMessageSender for TestSender {
        fn send_discord_message<'a>(
            &'a self,
            content: &'a str,
        ) -> Pin<Box<dyn Future<Output = Result<(), DiscordSendError>> + Send + 'a>> {
            Box::pin(async move {
                self.sent.lock().await.push(content.to_string());
                self.results.lock().await.pop_front().unwrap_or(Ok(()))
            })
        }
    }

    fn db_url() -> Option<String> {
        std::env::var("TEST_DATABASE_URL").ok().or_else(|| {
            eprintln!("SKIP: set TEST_DATABASE_URL for Discord monitor DB integration tests");
            None
        })
    }

    static DB_TEST_LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();

    async fn db_test_guard() -> tokio::sync::MutexGuard<'static, ()> {
        DB_TEST_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await
    }

    async fn test_pool() -> Option<PgPool> {
        let url = db_url()?;
        let pool = PgPoolOptions::new()
            .max_connections(3)
            .connect(&url)
            .await
            .ok()?;
        crate::db::run_migrations(&url).await.ok()?;
        Some(pool)
    }

    fn future_base() -> DateTime<Utc> {
        let offset = (Uuid::new_v4().as_u128() % 1_000_000) as i64;
        Utc::now() + Duration::days(365 * 20) + Duration::seconds(offset)
    }

    async fn seed_user(pool: &PgPool, label: &str) -> Uuid {
        let suffix = Uuid::new_v4();
        let entity_id: Uuid = sqlx::query_scalar(
            "INSERT INTO entities (name, domain, verification_method)
             VALUES ($1, $2, 'email')
             RETURNING id",
        )
        .bind(format!("Monitor {label}"))
        .bind(format!("monitor-{label}-{suffix}.test"))
        .fetch_one(pool)
        .await
        .expect("insert entity");

        sqlx::query_scalar(
            "INSERT INTO users (entity_id, display_name, email, auth_method)
             VALUES ($1, $2, $3, 'email')
             RETURNING id",
        )
        .bind(entity_id)
        .bind(format!("Monitor {label}"))
        .bind(format!("monitor-{label}-{suffix}@example.test"))
        .fetch_one(pool)
        .await
        .expect("insert user")
    }

    async fn seed_battle(
        pool: &PgPool,
        author_id: Uuid,
        label: &str,
        created_at: DateTime<Utc>,
    ) -> Uuid {
        let slug = format!("monitor-battle-{label}-{}", Uuid::new_v4());
        sqlx::query_scalar(
            "INSERT INTO battles (
                author_id, title, description, domain_tags, status, slug, created_at, updated_at
             )
             VALUES ($1, $2, $3, $4, 'open', $5, $6, $6)
             RETURNING id",
        )
        .bind(author_id)
        .bind(format!("Monitor battle {label}"))
        .bind("Disposable monitor test battle")
        .bind(vec!["monitor".to_string()])
        .bind(slug)
        .bind(created_at)
        .fetch_one(pool)
        .await
        .expect("insert battle")
    }

    async fn seed_claim(
        pool: &PgPool,
        author_id: Uuid,
        battle_id: Option<Uuid>,
        label: &str,
        created_at: DateTime<Utc>,
        visibility: &str,
    ) -> Uuid {
        let suffix = Uuid::new_v4();
        sqlx::query_scalar(
            "INSERT INTO claims (
                battle_id,
                author_id,
                claim,
                artifact_hash,
                artifact_url,
                artifact_size,
                created_at,
                updated_at,
                visibility,
                slug,
                topic,
                topic_tags
             )
             VALUES ($1, $2, $3, $4, $5, 1024, $6, $6, $7, $8, $9, $10)
             RETURNING id",
        )
        .bind(battle_id)
        .bind(author_id)
        .bind(format!(
            "# Monitor claim {label}\n\n## Summary\nDisposable test claim."
        ))
        .bind(format!("sha256:{suffix}"))
        .bind(format!("https://example.test/artifacts/{suffix}.tar.gz"))
        .bind(created_at)
        .bind(visibility)
        .bind(format!("monitor-claim-{label}-{suffix}"))
        .bind(if battle_id.is_none() {
            Some(format!("Monitor topic {label}"))
        } else {
            None
        })
        .bind(Vec::<String>::new())
        .fetch_one(pool)
        .await
        .expect("insert claim")
    }

    async fn seed_successor(
        pool: &PgPool,
        predecessor_id: Uuid,
        author_id: Uuid,
        battle_id: Option<Uuid>,
        label: &str,
        created_at: DateTime<Utc>,
    ) -> Uuid {
        let suffix = Uuid::new_v4();
        sqlx::query_scalar(
            "INSERT INTO claims (
                battle_id,
                author_id,
                claim,
                artifact_hash,
                artifact_url,
                artifact_size,
                predecessor_id,
                created_at,
                updated_at,
                visibility,
                slug,
                topic,
                topic_tags
             )
             VALUES ($1, $2, $3, $4, $5, 1024, $6, $7, $7, 'public', $8, $9, $10)
             RETURNING id",
        )
        .bind(battle_id)
        .bind(author_id)
        .bind(format!(
            "# Monitor successor {label}\n\n## Summary\nDisposable test claim."
        ))
        .bind(format!("sha256:{suffix}"))
        .bind(format!("https://example.test/artifacts/{suffix}.tar.gz"))
        .bind(predecessor_id)
        .bind(created_at)
        .bind(format!("monitor-successor-{label}-{suffix}"))
        .bind(if battle_id.is_none() {
            Some(format!("Monitor successor topic {label}"))
        } else {
            None
        })
        .bind(Vec::<String>::new())
        .fetch_one(pool)
        .await
        .expect("insert successor claim")
    }

    async fn seed_verification(
        pool: &PgPool,
        claim_id: Uuid,
        author_id: Uuid,
        verdict: &str,
        created_at: DateTime<Utc>,
    ) -> Uuid {
        sqlx::query_scalar(
            "INSERT INTO verifications (
                claim_id,
                author_id,
                verdict,
                environment,
                output_hash,
                duration_ms,
                failure_reason,
                conditions,
                reproduction_steps,
                created_at
             )
             VALUES ($1, $2, $3, 'linux x86_64', 'sha256:test', 42,
                     'did not reproduce', 'test conditions', 'test steps', $4)
             RETURNING id",
        )
        .bind(claim_id)
        .bind(author_id)
        .bind(verdict)
        .bind(created_at)
        .fetch_one(pool)
        .await
        .expect("insert verification")
    }

    #[test]
    fn enabled_requires_exact_true() {
        assert!(config_from(&[("DISCORD_MONITOR_ENABLED", "true")]).enabled);
        assert!(!config_from(&[("DISCORD_MONITOR_ENABLED", "TRUE")]).enabled);
        assert!(!config_from(&[("DISCORD_MONITOR_ENABLED", "1")]).enabled);
        assert!(!config_from(&[]).enabled);
    }

    #[test]
    fn sending_requires_enabled_bot_token_and_channel_id() {
        let ready = config_from(&[
            ("DISCORD_MONITOR_ENABLED", "true"),
            ("DISCORD_MONITOR_BOT_TOKEN", "bot-token"),
            ("DISCORD_MONITOR_CHANNEL_ID", "123"),
        ]);
        assert!(ready.can_send_to_discord());

        let no_token = config_from(&[
            ("DISCORD_MONITOR_ENABLED", "true"),
            ("DISCORD_MONITOR_BOT_TOKEN", "   "),
            ("DISCORD_MONITOR_CHANNEL_ID", "123"),
        ]);
        assert!(!no_token.can_send_to_discord());

        let no_channel = config_from(&[
            ("DISCORD_MONITOR_ENABLED", "true"),
            ("DISCORD_MONITOR_BOT_TOKEN", "bot-token"),
            ("DISCORD_MONITOR_CHANNEL_ID", "   "),
        ]);
        assert!(!no_channel.can_send_to_discord());
    }

    #[test]
    fn public_base_url_is_normalized() {
        let config = config_from(&[("PUBLIC_BASE_URL", " https://krabarena.com/// ")]);
        assert_eq!(config.public_base_url, "https://krabarena.com");

        let defaulted = config_from(&[("PUBLIC_BASE_URL", "   ")]);
        assert_eq!(defaulted.public_base_url, DEFAULT_PUBLIC_BASE_URL);
    }

    #[test]
    fn debug_output_redacts_secrets() {
        let config = config_from(&[
            ("DISCORD_MONITOR_BOT_TOKEN", "secret-bot-token"),
            ("CRON_SHARED_SECRET", "secret-cron-token"),
        ]);
        let debug = format!("{config:?}");
        assert!(!debug.contains("secret-bot-token"));
        assert!(!debug.contains("secret-cron-token"));
        assert!(debug.contains("has_bot_token"));
        assert!(debug.contains("has_cron_shared_secret"));

        let secret_debug = format!("{:?}", RedactedSecret::new("secret-value"));
        assert!(!secret_debug.contains("secret-value"));
    }

    #[test]
    fn sql_values_are_stable() {
        assert_eq!(CheckpointKind::NewClaims.as_str(), "new_claims");
        assert_eq!(
            CheckpointKind::NewVerifications.as_str(),
            "new_verifications"
        );
        assert_eq!(DiscordMonitorRunStatus::Success.as_str(), "success");
        assert_eq!(DiscordMonitorRunStatus::Disabled.as_str(), "disabled");
        assert_eq!(DiscordMonitorRunStatus::Failed.as_str(), "failed");
    }

    #[test]
    fn cron_auth_requires_configured_matching_secret() {
        let config = config_from(&[("CRON_SHARED_SECRET", "letmein")]);
        assert!(is_authorized_cron_request(&config, Some("letmein")));
        assert!(!is_authorized_cron_request(&config, Some("wrong")));
        assert!(!is_authorized_cron_request(&config, None));
        // Prefix / extension of the secret must not match (ct_eq path).
        assert!(!is_authorized_cron_request(&config, Some("letmei")));
        assert!(!is_authorized_cron_request(&config, Some("letmein!")));

        let empty = config_from(&[("CRON_SHARED_SECRET", "   ")]);
        assert!(!is_authorized_cron_request(&empty, Some("   ")));
    }

    #[test]
    fn run_status_reflects_config_readiness() {
        let ready = config_from(&[
            ("DISCORD_MONITOR_ENABLED", "true"),
            ("DISCORD_MONITOR_BOT_TOKEN", "bot-token"),
            ("DISCORD_MONITOR_CHANNEL_ID", "123"),
        ]);
        assert_eq!(
            determine_run_status(&ready),
            DiscordMonitorRunStatus::Success
        );

        let disabled = config_from(&[
            ("DISCORD_MONITOR_ENABLED", "false"),
            ("DISCORD_MONITOR_BOT_TOKEN", "bot-token"),
            ("DISCORD_MONITOR_CHANNEL_ID", "123"),
        ]);
        assert_eq!(
            determine_run_status(&disabled),
            DiscordMonitorRunStatus::Disabled
        );

        let missing_token = config_from(&[
            ("DISCORD_MONITOR_ENABLED", "true"),
            ("DISCORD_MONITOR_CHANNEL_ID", "123"),
        ]);
        assert_eq!(
            determine_run_status(&missing_token),
            DiscordMonitorRunStatus::Disabled
        );

        let missing_channel = config_from(&[
            ("DISCORD_MONITOR_ENABLED", "true"),
            ("DISCORD_MONITOR_BOT_TOKEN", "bot-token"),
        ]);
        assert_eq!(
            determine_run_status(&missing_channel),
            DiscordMonitorRunStatus::Disabled
        );
    }

    #[test]
    fn run_response_serializes_without_secrets() {
        let response = DiscordMonitorRunResponse {
            run_id: Uuid::nil(),
            status: "success",
            new_claims_count: 0,
            new_verifications_count: 0,
            unverified_claims_count: 0,
            messages_sent: 0,
        };
        let json = serde_json::to_string(&response).expect("response serializes");
        assert!(json.contains("\"run_id\""));
        assert!(json.contains("\"status\":\"success\""));
        assert!(json.contains("\"messages_sent\":0"));
        assert!(!json.contains("secret"));
        assert!(!json.contains("bot-token"));
    }

    #[test]
    fn formatter_returns_none_for_empty_rows() {
        assert!(format_new_claims_message("https://krabarena.com", &[]).is_none());
        assert!(format_new_verifications_message("https://krabarena.com", &[]).is_none());
        assert!(
            format_unverified_claims_message("https://krabarena.com", Utc::now(), &[]).is_none()
        );
    }

    #[test]
    fn new_claims_formatter_sanitizes_title_and_uses_slug_url() {
        let id = Uuid::new_v4();
        let event = sample_claim_event(
            id,
            Some("claim-slug"),
            "# Title | with **markdown**\n\n## Summary\nartifact_url should not appear",
        );
        let message =
            format_new_claims_message("https://krabarena.com/", &[event]).expect("message");

        assert!(message.content.contains("## 🦀 New Claims"));
        assert!(message.content.contains("**Count:** 1"));
        assert!(
            message
                .content
                .contains("https://krabarena.com/claims/claim-slug")
        );
        assert!(
            message
                .content
                .contains("[Battle Title](https://krabarena.com/battles/battle-slug)")
        );
        assert!(message.content.contains("> Author: Author Name"));
        assert!(message.content.contains("· 🕒 "));
        assert!(!message.content.contains("📝"));
        assert!(!message.content.contains("🏟️"));
        assert!(!message.content.contains("👤"));
        assert!(message.content.contains("Title with markdown"));
        assert!(!message.content.contains("```"));
        assert!(!message.content.contains("artifact_url"));
        assert!(!message.content.contains("Bot "));
        assert!(message.content.len() <= DISCORD_MONITOR_MESSAGE_LIMIT);
    }

    #[test]
    fn formatter_uses_uuid_url_for_missing_slug_and_standalone_label() {
        let id = Uuid::new_v4();
        let mut event = sample_claim_event(id, None, "# Standalone title");
        event.battle_id = None;
        event.battle_slug = None;
        event.battle_title = None;
        let message =
            format_new_claims_message("https://krabarena.com", &[event]).expect("message");

        assert!(
            message
                .content
                .contains(&format!("https://krabarena.com/claims/{id}"))
        );
        assert!(message.content.contains("Standalone"));
    }

    #[test]
    fn verification_formatter_maps_verdicts_and_count() {
        let claim_id = Uuid::new_v4();
        let events = vec![
            sample_verification_event(Uuid::new_v4(), claim_id, "confirmed"),
            sample_verification_event(Uuid::new_v4(), claim_id, "disconfirmed"),
        ];
        let message =
            format_new_verifications_message("https://krabarena.com", &events).expect("message");

        assert!(message.content.contains("## ✅ New Verifications"));
        assert!(message.content.contains("**Count:** 2"));
        assert!(message.content.contains("Verify"));
        assert!(message.content.contains("Refute"));
        assert!(message.content.contains("## ✅ New Verifications"));
        assert!(!message.content.contains("1. ✅"));
        assert!(!message.content.contains("❌"));
        assert!(
            message
                .content
                .contains("[Verification Battle](https://krabarena.com/battles/battle-slug)")
        );
        assert!(message.content.contains("> Verifier: Verifier"));
        assert!(message.content.contains("· 🕒 "));
        assert!(!message.content.contains("🏟️"));
        assert!(!message.content.contains("👤"));
        assert!(
            message
                .content
                .contains("https://krabarena.com/claims/verified-claim")
        );
        assert!(!message.content.contains("Full private markdown"));
    }

    #[test]
    fn unverified_formatter_reports_level_distribution_and_age() {
        let now = Utc::now();
        let claims = vec![
            sample_unverified_claim(
                Uuid::new_v4(),
                UnverifiedAlertLevel::TwelveHours,
                now - Duration::hours(13),
            ),
            sample_unverified_claim(
                Uuid::new_v4(),
                UnverifiedAlertLevel::TwentyFourHours,
                now - Duration::hours(25),
            ),
            sample_unverified_claim(
                Uuid::new_v4(),
                UnverifiedAlertLevel::FortyEightPlus,
                now - Duration::hours(49),
            ),
        ];
        let message = format_unverified_claims_message("https://krabarena.com", now, &claims)
            .expect("message");

        assert!(
            message
                .content
                .contains("Count: 3 total - 1 at 12h, 1 at 24h, 1 at 48h+")
        );
        assert!(
            message
                .content
                .contains("## ⏳ Claims without verification for 12h+")
        );
        assert!(message.content.contains("Battle: Standalone"));
        assert!(message.content.contains("> Level: **12h**"));
        assert!(message.content.contains("> Age: **13h**"));
        assert!(message.content.contains("> Author: Unverified Author"));
        assert!(!message.content.contains("Battle: Standalone · Author:"));
        assert!(!message.content.contains("1. ⏳"));
        assert!(!message.content.contains("🚨"));
        assert!(!message.content.contains("🏟️"));
        assert!(!message.content.contains("👤"));
        assert!(message.content.contains("13h"));
        assert!(message.content.contains("25h"));
        assert!(message.content.contains("49h"));
    }

    #[test]
    fn formatter_caps_rows_and_message_length() {
        let events = (0..25)
            .map(|idx| {
                sample_claim_event(
                    Uuid::new_v4(),
                    Some(&format!("claim-{idx}")),
                    &format!("# {}", "very long markdown title ".repeat(20)),
                )
            })
            .collect::<Vec<_>>();
        let message = format_new_claims_message("https://krabarena.com", &events).expect("message");

        assert!(message.included_rows <= DISCORD_MONITOR_BATCH_LIMIT as usize);
        assert_eq!(message.total_rows, 25);
        assert!(message.content.contains("and "));
        assert!(message.content.len() <= DISCORD_MONITOR_MESSAGE_LIMIT);
    }

    #[tokio::test]
    async fn discord_client_sends_expected_request() {
        let (base_url, requests) =
            spawn_discord_mock(vec![MockDiscordResponse::new(StatusCode::OK, None, "{}")]).await;
        let client = DiscordBotClient::with_base_url("secret-token", "channel-123", &base_url);

        client.send_message("hello discord").await.expect("send");

        let captured = requests.lock().await.clone();
        assert_eq!(captured.len(), 1);
        assert_eq!(captured[0].channel_id, "channel-123");
        assert_eq!(
            captured[0].authorization.as_deref(),
            Some("Bot secret-token")
        );
        assert_eq!(captured[0].body["content"], "hello discord");
    }

    #[tokio::test]
    async fn discord_client_rejects_too_long_payload_before_request() {
        let (base_url, requests) =
            spawn_discord_mock(vec![MockDiscordResponse::new(StatusCode::OK, None, "{}")]).await;
        let client = DiscordBotClient::with_base_url("secret-token", "channel-123", &base_url);

        let error = client
            .send_message(&"a".repeat(DISCORD_MONITOR_MESSAGE_LIMIT + 1))
            .await
            .expect_err("payload rejected");

        assert_eq!(error.kind, DiscordSendErrorKind::PayloadValidation);
        assert_eq!(error.attempts, 0);
        assert!(requests.lock().await.is_empty());
    }

    #[tokio::test]
    async fn discord_client_maps_non_retryable_statuses() {
        for (status, kind) in [
            (
                StatusCode::BAD_REQUEST,
                DiscordSendErrorKind::PayloadValidation,
            ),
            (
                StatusCode::UNAUTHORIZED,
                DiscordSendErrorKind::BotAuthorization,
            ),
            (StatusCode::FORBIDDEN, DiscordSendErrorKind::BotPermissions),
            (StatusCode::NOT_FOUND, DiscordSendErrorKind::ChannelNotFound),
        ] {
            let (base_url, requests) =
                spawn_discord_mock(vec![MockDiscordResponse::new(status, None, "{}")]).await;
            let client = DiscordBotClient::with_base_url("secret-token", "channel-123", &base_url);
            let error = client.send_message("hello").await.expect_err("send fails");

            assert_eq!(error.kind, kind);
            assert_eq!(error.attempts, 1);
            assert_eq!(requests.lock().await.len(), 1);
            assert!(!format!("{error:?}").contains("secret-token"));
            assert!(!error.sanitized_message().contains("hello"));
        }
    }

    #[tokio::test]
    async fn discord_client_retries_429_retry_after_header() {
        let (base_url, requests) = spawn_discord_mock(vec![
            MockDiscordResponse::new(StatusCode::TOO_MANY_REQUESTS, Some("0"), "{}"),
            MockDiscordResponse::new(StatusCode::OK, None, "{}"),
        ])
        .await;
        let client = DiscordBotClient::with_base_url("secret-token", "channel-123", &base_url);

        client.send_message("hello").await.expect("retry succeeds");

        assert_eq!(requests.lock().await.len(), 2);
    }

    #[tokio::test]
    async fn discord_client_retries_429_retry_after_json_and_then_fails() {
        let (base_url, requests) = spawn_discord_mock(vec![
            MockDiscordResponse::new(StatusCode::TOO_MANY_REQUESTS, None, r#"{"retry_after":0}"#),
            MockDiscordResponse::new(StatusCode::TOO_MANY_REQUESTS, None, r#"{"retry_after":0}"#),
            MockDiscordResponse::new(StatusCode::TOO_MANY_REQUESTS, None, r#"{"retry_after":0}"#),
        ])
        .await;
        let client = DiscordBotClient::with_base_url("secret-token", "channel-123", &base_url);
        let error = client.send_message("hello").await.expect_err("retry fails");

        assert_eq!(error.kind, DiscordSendErrorKind::RateLimited);
        assert_eq!(error.attempts, 3);
        assert_eq!(requests.lock().await.len(), 3);
    }

    #[tokio::test]
    async fn discord_client_retries_5xx_and_succeeds() {
        let (base_url, requests) = spawn_discord_mock(vec![
            MockDiscordResponse::new(StatusCode::INTERNAL_SERVER_ERROR, None, "{}"),
            MockDiscordResponse::new(StatusCode::OK, None, "{}"),
        ])
        .await;
        let client = DiscordBotClient::with_base_url("secret-token", "channel-123", &base_url);

        client.send_message("hello").await.expect("retry succeeds");

        assert_eq!(requests.lock().await.len(), 2);
    }

    #[tokio::test]
    async fn discord_client_retries_network_error() {
        let client = DiscordBotClient::with_base_url(
            "secret-token",
            "channel-123",
            "http://127.0.0.1:1/api/v10",
        );
        let error = client
            .send_message("hello")
            .await
            .expect_err("network send fails");

        assert_eq!(error.kind, DiscordSendErrorKind::NetworkError);
        assert_eq!(error.attempts, 3);
    }

    #[tokio::test]
    async fn discord_client_retries_timeout() {
        let (base_url, requests) = spawn_discord_mock(vec![
            MockDiscordResponse::delayed(StatusCode::OK, StdDuration::from_millis(50)),
            MockDiscordResponse::delayed(StatusCode::OK, StdDuration::from_millis(50)),
            MockDiscordResponse::delayed(StatusCode::OK, StdDuration::from_millis(50)),
        ])
        .await;
        let client = DiscordBotClient::with_base_url_and_timeout(
            "secret-token",
            "channel-123",
            &base_url,
            StdDuration::from_millis(1),
        );

        let error = client
            .send_message("hello")
            .await
            .expect_err("timeout send fails");

        assert_eq!(error.kind, DiscordSendErrorKind::Timeout);
        assert_eq!(error.attempts, 3);
        assert!(!format!("{error:?}").contains("secret-token"));
        assert!(!error.sanitized_message().contains("hello"));
        assert!(!requests.lock().await.is_empty());
    }

    #[tokio::test]
    async fn enabled_monitoring_run_sends_messages_and_updates_state() {
        let _guard = db_test_guard().await;
        let Some(pool) = test_pool().await else {
            return;
        };

        let author_id = seed_user(&pool, "run-success").await;
        let now = future_base();
        save_checkpoint_after_sent(
            &pool,
            CheckpointKind::NewClaims,
            now - Duration::milliseconds(1),
            Uuid::nil(),
        )
        .await
        .expect("seed claim checkpoint");
        save_checkpoint_after_sent(
            &pool,
            CheckpointKind::NewVerifications,
            now - Duration::milliseconds(1),
            Uuid::nil(),
        )
        .await
        .expect("seed verification checkpoint");

        let new_claim = seed_claim(&pool, author_id, None, "run-new", now, "public").await;
        let verified_claim =
            seed_claim(&pool, author_id, None, "run-verified", now, "public").await;
        let verification =
            seed_verification(&pool, verified_claim, author_id, "confirmed", now).await;
        let unverified_claim = seed_claim(
            &pool,
            author_id,
            None,
            "run-unverified",
            now - Duration::hours(13),
            "public",
        )
        .await;
        let config = config_from(&[
            ("DISCORD_MONITOR_ENABLED", "true"),
            ("DISCORD_MONITOR_BOT_TOKEN", "secret-token"),
            ("DISCORD_MONITOR_CHANNEL_ID", "123"),
            ("PUBLIC_BASE_URL", "https://krabarena.com"),
        ]);
        let sender = TestSender::success();

        let counters = run_enabled_monitoring(&pool, &config, &sender, now)
            .await
            .expect("run monitoring");

        assert_eq!(counters.messages_sent, 3);
        assert_eq!(counters.new_claims_count, 2);
        assert_eq!(counters.new_verifications_count, 1);
        assert_eq!(counters.unverified_claims_count, 1);

        let sent = sender.sent.lock().await.clone();
        assert_eq!(sent.len(), 3);
        assert!(sent[0].contains("New Claims"));
        assert!(sent[1].contains("New Verifications"));
        assert!(sent[2].contains("Claims without verification"));
        assert!(!sent.iter().any(|message| message.contains("secret-token")));

        let claim_checkpoint = load_checkpoint(&pool, CheckpointKind::NewClaims)
            .await
            .expect("load claim checkpoint")
            .expect("claim checkpoint");
        assert!(
            claim_checkpoint.last_id == Some(new_claim)
                || claim_checkpoint.last_id == Some(verified_claim)
        );

        let verification_checkpoint = load_checkpoint(&pool, CheckpointKind::NewVerifications)
            .await
            .expect("load verification checkpoint")
            .expect("verification checkpoint");
        assert_eq!(verification_checkpoint.last_id, Some(verification));

        let alert_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM discord_monitor_claim_alerts
             WHERE claim_id = $1 AND alert_count = 1 AND resolved_at IS NULL",
        )
        .bind(unverified_claim)
        .fetch_one(&pool)
        .await
        .expect("count unverified alert");
        assert_eq!(alert_count, 1);
    }

    #[tokio::test]
    async fn failed_discord_send_does_not_move_checkpoint() {
        let _guard = db_test_guard().await;
        let Some(pool) = test_pool().await else {
            return;
        };

        let author_id = seed_user(&pool, "run-failed").await;
        let now = future_base();
        save_checkpoint_after_sent(
            &pool,
            CheckpointKind::NewClaims,
            now - Duration::milliseconds(1),
            Uuid::nil(),
        )
        .await
        .expect("seed claim checkpoint");
        seed_claim(&pool, author_id, None, "run-failed-new", now, "public").await;
        let config = config_from(&[
            ("DISCORD_MONITOR_ENABLED", "true"),
            ("DISCORD_MONITOR_BOT_TOKEN", "secret-token"),
            ("DISCORD_MONITOR_CHANNEL_ID", "123"),
            ("PUBLIC_BASE_URL", "https://krabarena.com"),
        ]);
        let sender = TestSender::with_results(vec![Err(DiscordSendError::new(
            DiscordSendErrorKind::DiscordUnavailable,
            Some(500),
            None,
            3,
        ))]);

        let error = run_enabled_monitoring(&pool, &config, &sender, now)
            .await
            .expect_err("run fails");

        assert_eq!(error.counters.messages_sent, 0);
        assert!(error.sanitized_message().contains("discord_unavailable"));
        assert!(!error.sanitized_message().contains("secret-token"));

        let checkpoint = load_checkpoint(&pool, CheckpointKind::NewClaims)
            .await
            .expect("load claim checkpoint")
            .expect("claim checkpoint");
        assert_eq!(checkpoint.last_id, Some(Uuid::nil()));
        assert_eq!(
            checkpoint.last_created_at,
            Some(now - Duration::milliseconds(1))
        );
    }

    #[test]
    fn migration_creates_required_tables() {
        assert!(MIGRATION_SQL.contains("CREATE TABLE IF NOT EXISTS discord_monitor_checkpoints"));
        assert!(MIGRATION_SQL.contains("CREATE TABLE IF NOT EXISTS discord_monitor_claim_alerts"));
        assert!(MIGRATION_SQL.contains("CREATE TABLE IF NOT EXISTS discord_monitor_runs"));
    }

    #[test]
    fn migration_constrains_checkpoint_kind_and_alert_state() {
        assert!(MIGRATION_SQL.contains("kind TEXT PRIMARY KEY"));
        assert!(MIGRATION_SQL.contains("kind IN ('new_claims', 'new_verifications')"));
        assert!(MIGRATION_SQL.contains("claim_id UUID PRIMARY KEY REFERENCES claims(id)"));
        assert!(MIGRATION_SQL.contains("alert_count INT NOT NULL DEFAULT 0"));
        assert!(MIGRATION_SQL.contains("CHECK (alert_count >= 0)"));
        assert!(MIGRATION_SQL.contains("last_alert_level IN ('12h', '24h', '48h+')"));
    }

    #[test]
    fn migration_constrains_run_status_and_counters() {
        assert!(MIGRATION_SQL.contains("status TEXT NOT NULL"));
        assert!(MIGRATION_SQL.contains("status IN ('success', 'disabled', 'failed')"));
        assert!(MIGRATION_SQL.contains("CHECK (new_claims_count >= 0)"));
        assert!(MIGRATION_SQL.contains("CHECK (new_verifications_count >= 0)"));
        assert!(MIGRATION_SQL.contains("CHECK (unverified_claims_count >= 0)"));
        assert!(MIGRATION_SQL.contains("CHECK (messages_sent >= 0)"));
    }

    #[tokio::test]
    async fn claim_events_use_checkpoint_limit_and_stable_order() {
        let _guard = db_test_guard().await;
        let Some(pool) = test_pool().await else {
            return;
        };

        let author_id = seed_user(&pool, "claim-batch").await;
        let base = future_base();
        let battle_id = seed_battle(&pool, author_id, "claim-batch", base).await;
        let checkpoint = save_checkpoint_after_sent(
            &pool,
            CheckpointKind::NewClaims,
            base - Duration::milliseconds(1),
            Uuid::nil(),
        )
        .await
        .expect("seed initial checkpoint");

        let mut expected = Vec::new();
        for idx in 0..25 {
            let created_at = base + Duration::milliseconds(idx);
            let id = seed_claim(
                &pool,
                author_id,
                Some(battle_id),
                &format!("batch-{idx}"),
                created_at,
                "public",
            )
            .await;
            expected.push((created_at, id));
        }

        let first = fetch_new_claim_events_after(&pool, Some(&checkpoint))
            .await
            .expect("fetch first claim batch");
        assert_eq!(first.len(), DISCORD_MONITOR_BATCH_LIMIT as usize);
        assert_eq!(
            first.iter().map(|event| event.id).collect::<Vec<_>>(),
            expected
                .iter()
                .take(DISCORD_MONITOR_BATCH_LIMIT as usize)
                .map(|(_, id)| *id)
                .collect::<Vec<_>>()
        );

        let after_read = load_checkpoint(&pool, CheckpointKind::NewClaims)
            .await
            .expect("load checkpoint after read")
            .expect("checkpoint exists");
        assert_eq!(after_read.last_id, Some(Uuid::nil()));
        assert_eq!(
            after_read.last_created_at,
            Some(base - Duration::milliseconds(1))
        );

        let last_sent = first.last().expect("first batch has last event");
        let saved = save_checkpoint_after_sent(
            &pool,
            CheckpointKind::NewClaims,
            last_sent.created_at,
            last_sent.id,
        )
        .await
        .expect("save checkpoint");
        assert_eq!(saved.last_id, Some(last_sent.id));

        let remaining = fetch_new_claim_events(&pool)
            .await
            .expect("fetch remaining claim batch");
        assert_eq!(
            remaining.iter().map(|event| event.id).collect::<Vec<_>>(),
            expected
                .iter()
                .skip(DISCORD_MONITOR_BATCH_LIMIT as usize)
                .map(|(_, id)| *id)
                .collect::<Vec<_>>()
        );
    }

    #[tokio::test]
    async fn claim_events_include_private_standalone_and_battle_but_exclude_deleted_and_superseded()
    {
        let _guard = db_test_guard().await;
        let Some(pool) = test_pool().await else {
            return;
        };

        let author_id = seed_user(&pool, "claim-filters").await;
        let base = future_base();
        let battle_id = seed_battle(&pool, author_id, "claim-filters", base).await;
        let checkpoint = DiscordMonitorCheckpoint {
            kind: CheckpointKind::NewClaims,
            last_created_at: Some(base - Duration::milliseconds(1)),
            last_id: Some(Uuid::nil()),
            updated_at: base,
        };

        let battle_claim = seed_claim(
            &pool,
            author_id,
            Some(battle_id),
            "filter-battle",
            base,
            "public",
        )
        .await;
        let standalone_claim =
            seed_claim(&pool, author_id, None, "filter-standalone", base, "public").await;
        let private_claim =
            seed_claim(&pool, author_id, None, "filter-private", base, "private").await;
        let deleted_claim =
            seed_claim(&pool, author_id, None, "filter-deleted", base, "public").await;
        sqlx::query("UPDATE claims SET deleted_at = $1 WHERE id = $2")
            .bind(base)
            .bind(deleted_claim)
            .execute(&pool)
            .await
            .expect("soft-delete claim");
        let superseded_claim =
            seed_claim(&pool, author_id, None, "filter-superseded", base, "public").await;
        let successor = seed_successor(
            &pool,
            superseded_claim,
            author_id,
            None,
            "filter-successor",
            base,
        )
        .await;

        let events = fetch_new_claim_events_after(&pool, Some(&checkpoint))
            .await
            .expect("fetch filtered claims");
        let ids = events.iter().map(|event| event.id).collect::<Vec<_>>();

        assert!(ids.contains(&battle_claim));
        assert!(ids.contains(&standalone_claim));
        assert!(ids.contains(&private_claim));
        assert!(ids.contains(&successor));
        assert!(!ids.contains(&deleted_claim));
        assert!(!ids.contains(&superseded_claim));
    }

    #[tokio::test]
    async fn verification_events_use_checkpoint_and_exclude_deleted_or_superseded_claims() {
        let _guard = db_test_guard().await;
        let Some(pool) = test_pool().await else {
            return;
        };

        let author_id = seed_user(&pool, "verification-events").await;
        let base = future_base();
        let live_claim = seed_claim(&pool, author_id, None, "verify-live", base, "public").await;
        let deleted_claim =
            seed_claim(&pool, author_id, None, "verify-deleted", base, "public").await;
        sqlx::query("UPDATE claims SET deleted_at = $1 WHERE id = $2")
            .bind(base)
            .bind(deleted_claim)
            .execute(&pool)
            .await
            .expect("soft-delete claim");
        let superseded_claim =
            seed_claim(&pool, author_id, None, "verify-superseded", base, "public").await;
        seed_successor(
            &pool,
            superseded_claim,
            author_id,
            None,
            "verify-successor",
            base + Duration::milliseconds(1),
        )
        .await;

        let confirmed = seed_verification(&pool, live_claim, author_id, "confirmed", base).await;
        let disconfirmed = seed_verification(
            &pool,
            live_claim,
            author_id,
            "disconfirmed",
            base + Duration::milliseconds(1),
        )
        .await;
        let deleted_verification = seed_verification(
            &pool,
            deleted_claim,
            author_id,
            "confirmed",
            base + Duration::milliseconds(2),
        )
        .await;
        let superseded_verification = seed_verification(
            &pool,
            superseded_claim,
            author_id,
            "confirmed",
            base + Duration::milliseconds(3),
        )
        .await;

        let checkpoint = save_checkpoint_after_sent(
            &pool,
            CheckpointKind::NewVerifications,
            base - Duration::milliseconds(1),
            Uuid::nil(),
        )
        .await
        .expect("seed verification checkpoint");
        let events = fetch_new_verification_events_after(&pool, Some(&checkpoint))
            .await
            .expect("fetch verification events");
        let ids = events.iter().map(|event| event.id).collect::<Vec<_>>();

        assert_eq!(ids, vec![confirmed, disconfirmed]);
        assert!(!ids.contains(&deleted_verification));
        assert!(!ids.contains(&superseded_verification));
        let after_read = load_checkpoint(&pool, CheckpointKind::NewVerifications)
            .await
            .expect("load checkpoint after read")
            .expect("checkpoint exists");
        assert_eq!(after_read.last_id, Some(Uuid::nil()));
        assert_eq!(
            after_read.last_created_at,
            Some(base - Duration::milliseconds(1))
        );
    }

    #[tokio::test]
    async fn unverified_claims_repeat_after_24h_and_resolve_after_verification() {
        let _guard = db_test_guard().await;
        let Some(pool) = test_pool().await else {
            return;
        };

        let author_id = seed_user(&pool, "unverified-repeat").await;
        let now = future_base();
        let old_claim = seed_claim(
            &pool,
            author_id,
            None,
            "unverified-old",
            now - Duration::hours(13),
            "public",
        )
        .await;
        let verified_claim = seed_claim(
            &pool,
            author_id,
            None,
            "unverified-verified",
            now - Duration::hours(13),
            "public",
        )
        .await;
        seed_verification(&pool, verified_claim, author_id, "confirmed", now).await;
        let refuted_claim = seed_claim(
            &pool,
            author_id,
            None,
            "unverified-refuted",
            now - Duration::hours(13),
            "public",
        )
        .await;
        seed_verification(&pool, refuted_claim, author_id, "disconfirmed", now).await;
        seed_claim(
            &pool,
            author_id,
            None,
            "unverified-fresh",
            now - Duration::hours(2),
            "public",
        )
        .await;

        let initial = fetch_unverified_claims(&pool, now)
            .await
            .expect("fetch initial unverified");
        assert_eq!(
            initial.iter().map(|claim| claim.id).collect::<Vec<_>>(),
            vec![old_claim]
        );
        assert_eq!(initial[0].alert_level, UnverifiedAlertLevel::TwelveHours);

        let alert = record_unverified_alert_sent(&pool, old_claim, initial[0].alert_level, now)
            .await
            .expect("record alert");
        assert_eq!(alert.alert_count, 1);
        assert_eq!(alert.last_alert_level.as_deref(), Some("12h"));

        let before_repeat = fetch_unverified_claims(&pool, now + Duration::hours(23))
            .await
            .expect("fetch before repeat window");
        assert!(!before_repeat.iter().any(|claim| claim.id == old_claim));

        let repeat =
            fetch_unverified_claims(&pool, now + Duration::hours(24) + Duration::seconds(1))
                .await
                .expect("fetch repeat unverified");
        let repeated = repeat
            .iter()
            .find(|claim| claim.id == old_claim)
            .expect("old claim repeats after 24h");
        assert_eq!(repeated.alert_count, 1);
        assert_eq!(repeated.alert_level, UnverifiedAlertLevel::TwentyFourHours);

        seed_verification(
            &pool,
            old_claim,
            author_id,
            "confirmed",
            now + Duration::hours(25),
        )
        .await;
        let resolved = resolve_completed_claim_alerts(&pool, now + Duration::hours(25))
            .await
            .expect("resolve completed alerts");
        assert!(resolved >= 1);

        let after_resolve = fetch_unverified_claims(&pool, now + Duration::hours(48))
            .await
            .expect("fetch after resolve");
        assert!(!after_resolve.iter().any(|claim| claim.id == old_claim));
    }

    #[tokio::test]
    async fn resolve_completed_alerts_handles_soft_delete_and_superseded_claims() {
        let _guard = db_test_guard().await;
        let Some(pool) = test_pool().await else {
            return;
        };

        let author_id = seed_user(&pool, "resolve-alerts").await;
        let now = future_base();
        let deleted_claim = seed_claim(
            &pool,
            author_id,
            None,
            "resolve-deleted",
            now - Duration::hours(13),
            "public",
        )
        .await;
        let superseded_claim = seed_claim(
            &pool,
            author_id,
            None,
            "resolve-superseded",
            now - Duration::hours(13),
            "public",
        )
        .await;
        let verified_claim = seed_claim(
            &pool,
            author_id,
            None,
            "resolve-verified",
            now - Duration::hours(13),
            "public",
        )
        .await;

        for claim_id in [deleted_claim, superseded_claim, verified_claim] {
            record_unverified_alert_sent(&pool, claim_id, UnverifiedAlertLevel::TwelveHours, now)
                .await
                .expect("record alert");
        }

        sqlx::query("UPDATE claims SET deleted_at = $1 WHERE id = $2")
            .bind(now)
            .bind(deleted_claim)
            .execute(&pool)
            .await
            .expect("soft-delete claim");
        seed_successor(
            &pool,
            superseded_claim,
            author_id,
            None,
            "resolve-successor",
            now,
        )
        .await;
        seed_verification(&pool, verified_claim, author_id, "confirmed", now).await;

        let resolved = resolve_completed_claim_alerts(&pool, now + Duration::minutes(1))
            .await
            .expect("resolve completed alerts");
        assert_eq!(resolved, 3);

        let unresolved_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*)
             FROM discord_monitor_claim_alerts
             WHERE claim_id = ANY($1) AND resolved_at IS NULL",
        )
        .bind(vec![deleted_claim, superseded_claim, verified_claim])
        .fetch_one(&pool)
        .await
        .expect("count unresolved alerts");
        assert_eq!(unresolved_count, 0);
    }
}

use serde::{Deserialize, Serialize};

/// Lifecycle phase of a Battle.
///
/// As of v0.5.0 this is a clean three-value lifecycle (was previously
/// open/contested/holding/settled). The "is this debate heated"
/// signal lives on `Battle.temperature` instead — keeping status
/// orthogonal to intensity. Old DB rows are migrated in-place by
/// `019_temperature_and_status.sql`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BattleStatus {
    /// Curated, no claims yet.
    Open,
    /// Has at least one claim. Accepts more.
    Active,
    /// Closed by an editor — terminal state.
    Settled,
}

impl BattleStatus {
    /// Translate a string from any caller (DB, URL param, API body)
    /// into the canonical enum, accepting the legacy `contested` /
    /// `holding` aliases so old bookmarks and API clients still work.
    pub fn parse_lenient(s: &str) -> Option<Self> {
        match s {
            "open" => Some(Self::Open),
            "active" | "contested" | "holding" => Some(Self::Active),
            "settled" => Some(Self::Settled),
            _ => None,
        }
    }
}

impl std::fmt::Display for BattleStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Open => write!(f, "open"),
            Self::Active => write!(f, "active"),
            Self::Settled => write!(f, "settled"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AuthMethod {
    Email,
    Dns,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VerificationMethod {
    Email,
    Dns,
}

/// Agent verdict on a Claim after running its artifact.
///
/// On the wire / in Rust: `"verified"` / `"refuted"`.
/// In the database the legacy values `'confirmed'` / `'disconfirmed'` are
/// still stored — conversion happens in [`Verdict::from_db`] /
/// [`Verdict::as_db`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Verdict {
    Verified,
    Refuted,
}

impl Verdict {
    pub fn from_db(value: &str) -> Option<Self> {
        match value {
            "confirmed" | "verified" => Some(Self::Verified),
            "disconfirmed" | "refuted" => Some(Self::Refuted),
            _ => None,
        }
    }

    pub fn as_db(&self) -> &'static str {
        match self {
            Self::Verified => "confirmed",
            Self::Refuted => "disconfirmed",
        }
    }
}

impl std::fmt::Display for Verdict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Verified => write!(f, "verified"),
            Self::Refuted => write!(f, "refuted"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Admin,
    Moderator,
    Editor,
    User,
}

impl Role {
    pub fn from_db(value: &str) -> Option<Self> {
        match value {
            "admin" => Some(Self::Admin),
            "moderator" => Some(Self::Moderator),
            "editor" => Some(Self::Editor),
            "user" => Some(Self::User),
            _ => None,
        }
    }

    pub fn as_db(&self) -> &'static str {
        match self {
            Self::Admin => "admin",
            Self::Moderator => "moderator",
            Self::Editor => "editor",
            Self::User => "user",
        }
    }

    /// Whether this role can open Battles.
    pub fn can_open_battles(&self) -> bool {
        matches!(self, Self::Editor | Self::Admin)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ClaimVisibility {
    Public,
    Private,
}

impl std::fmt::Display for ClaimVisibility {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Public => write!(f, "public"),
            Self::Private => write!(f, "private"),
        }
    }
}

use krabarena_shared::field_limits::{Field, truncate_to_cap};
use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

use crate::error::ApiError;
use crate::services::profile_links::{
    SocialSite, normalize_social, normalize_text, normalize_website,
};

const MAX_OAUTH_AVATAR_CHARS: usize = 1_500;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProfileField {
    FullName,
    Bio,
    Website,
    X,
    Linkedin,
    Github,
    Avatar,
}

impl ProfileField {
    pub const ALL: [ProfileField; 7] = [
        Self::FullName,
        Self::Bio,
        Self::Website,
        Self::X,
        Self::Linkedin,
        Self::Github,
        Self::Avatar,
    ];

    pub fn key(self) -> &'static str {
        match self {
            Self::FullName => "full_name",
            Self::Bio => "bio",
            Self::Website => "website_url",
            Self::X => "x_url",
            Self::Linkedin => "linkedin_url",
            Self::Github => "github_url",
            Self::Avatar => "avatar",
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, utoipa::ToSchema)]
pub struct PublicProfile {
    pub full_name: Option<String>,
    pub bio: Option<String>,
    pub website_url: Option<String>,
    pub x_url: Option<String>,
    pub linkedin_url: Option<String>,
    pub github_url: Option<String>,
    pub avatar_url: Option<String>,
}

impl PublicProfile {
    pub fn get(&self, field: ProfileField) -> Option<&str> {
        match field {
            ProfileField::FullName => self.full_name.as_deref(),
            ProfileField::Bio => self.bio.as_deref(),
            ProfileField::Website => self.website_url.as_deref(),
            ProfileField::X => self.x_url.as_deref(),
            ProfileField::Linkedin => self.linkedin_url.as_deref(),
            ProfileField::Github => self.github_url.as_deref(),
            ProfileField::Avatar => self.avatar_url.as_deref(),
        }
    }

    fn slot(&mut self, field: ProfileField) -> &mut Option<String> {
        match field {
            ProfileField::FullName => &mut self.full_name,
            ProfileField::Bio => &mut self.bio,
            ProfileField::Website => &mut self.website_url,
            ProfileField::X => &mut self.x_url,
            ProfileField::Linkedin => &mut self.linkedin_url,
            ProfileField::Github => &mut self.github_url,
            ProfileField::Avatar => &mut self.avatar_url,
        }
    }

    pub fn is_empty(&self) -> bool {
        ProfileField::ALL.iter().all(|f| self.get(*f).is_none())
    }

    pub fn has_details(&self) -> bool {
        ProfileField::ALL
            .iter()
            .filter(|f| **f != ProfileField::Avatar)
            .any(|f| self.get(*f).is_some())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProfileState {
    pub profile: PublicProfile,
    pub edited: Vec<String>,
    pub oauth: Vec<String>,
}

impl ProfileState {
    fn is_edited(&self, field: ProfileField) -> bool {
        self.edited.iter().any(|k| k == field.key())
    }

    fn is_oauth(&self, field: ProfileField) -> bool {
        self.oauth.iter().any(|k| k == field.key())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProfileEdit {
    pub changes: Vec<(ProfileField, Option<String>)>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ProfileUpdateRequest {
    #[serde(default, alias = "name")]
    pub full_name: Option<String>,
    #[serde(default)]
    pub bio: Option<String>,
    #[serde(default, alias = "website_url")]
    pub website: Option<String>,
    #[serde(default, alias = "x_url", alias = "twitter")]
    pub x: Option<String>,
    #[serde(default, alias = "linkedin_url")]
    pub linkedin: Option<String>,
    #[serde(default, alias = "github_url")]
    pub github: Option<String>,
}

fn check_len(field: Field, value: Option<String>) -> Result<Option<String>, ApiError> {
    if let Some(v) = value.as_deref() {
        field.check(v)?;
    }
    Ok(value)
}

impl ProfileUpdateRequest {
    pub fn into_edit(self) -> Result<ProfileEdit, ApiError> {
        let invalid = ApiError::ValidationError;
        let mut changes = Vec::new();
        if let Some(v) = self.full_name {
            let v = normalize_text("full_name", &v, false).map_err(invalid)?;
            changes.push((
                ProfileField::FullName,
                check_len(Field::UserProfilesFullName, v)?,
            ));
        }
        if let Some(v) = self.bio {
            let v = normalize_text("bio", &v, true).map_err(invalid)?;
            changes.push((ProfileField::Bio, check_len(Field::UserProfilesBio, v)?));
        }
        if let Some(v) = self.website {
            let v = normalize_website(&v).map_err(invalid)?;
            changes.push((
                ProfileField::Website,
                check_len(Field::UserProfilesWebsiteUrl, v)?,
            ));
        }
        if let Some(v) = self.x {
            let v = normalize_social(SocialSite::X, &v).map_err(invalid)?;
            changes.push((ProfileField::X, check_len(Field::UserProfilesXUrl, v)?));
        }
        if let Some(v) = self.linkedin {
            let v = normalize_social(SocialSite::Linkedin, &v).map_err(invalid)?;
            changes.push((
                ProfileField::Linkedin,
                check_len(Field::UserProfilesLinkedinUrl, v)?,
            ));
        }
        if let Some(v) = self.github {
            let v = normalize_social(SocialSite::Github, &v).map_err(invalid)?;
            changes.push((
                ProfileField::Github,
                check_len(Field::UserProfilesGithubUrl, v)?,
            ));
        }
        if changes.is_empty() {
            return Err(ApiError::ValidationError(
                "nothing to update: pass at least one of full_name, bio, website, x, linkedin, github"
                    .into(),
            ));
        }
        Ok(ProfileEdit { changes })
    }
}

pub fn apply_edit(current: &ProfileState, edit: &ProfileEdit) -> ProfileState {
    let mut next = current.clone();
    for (field, value) in &edit.changes {
        *next.profile.slot(*field) = value.clone();
        if !next.is_edited(*field) {
            next.edited.push(field.key().to_string());
        }
        next.oauth.retain(|k| k != field.key());
    }
    next
}

pub fn plan_seed(current: &ProfileState, seed: &PublicProfile) -> Option<ProfileState> {
    let mut next = current.clone();
    let mut changed = false;
    for field in ProfileField::ALL {
        let Some(value) = seed.get(field) else {
            continue;
        };
        if next.is_edited(field) {
            continue;
        }
        let current_value = next.profile.get(field);
        if current_value.is_some() && !next.is_oauth(field) {
            continue;
        }
        if current_value == Some(value) {
            continue;
        }
        *next.profile.slot(field) = Some(value.to_string());
        if !next.is_oauth(field) {
            next.oauth.push(field.key().to_string());
        }
        changed = true;
    }
    changed.then_some(next)
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OAuthProfile {
    pub provider: String,
    pub email: Option<String>,
    pub email_verified: bool,
    pub full_name: Option<String>,
    pub given_name: Option<String>,
    pub family_name: Option<String>,
    pub username: Option<String>,
    pub profile_url: Option<String>,
    pub avatar_url: Option<String>,
    pub website_url: Option<String>,
    pub x_handle: Option<String>,
    pub bio: Option<String>,
    pub company: Option<String>,
    pub location: Option<String>,
}

fn clip(field: Field, value: Option<String>) -> Option<String> {
    value.map(|v| truncate_to_cap(&v, field.max()).0)
}

pub fn clean_oauth_text(value: Option<&str>) -> Option<String> {
    value.and_then(|v| normalize_text("oauth", v, false).ok().flatten())
}

pub fn clean_oauth_avatar(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|v| v.starts_with("https://") && v.chars().count() <= MAX_OAUTH_AVATAR_CHARS)
        .map(str::to_string)
}

impl OAuthProfile {
    pub fn display_full_name(&self) -> Option<String> {
        self.full_name.clone().or_else(|| {
            let joined = [self.given_name.as_deref(), self.family_name.as_deref()]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join(" ");
            clean_oauth_text(Some(&joined))
        })
    }

    pub fn seed(&self) -> PublicProfile {
        let github_url = if self.provider == "github" {
            self.username
                .as_deref()
                .and_then(|u| normalize_social(SocialSite::Github, u).ok().flatten())
        } else {
            None
        };
        PublicProfile {
            full_name: clip(Field::UserProfilesFullName, self.display_full_name()),
            bio: clip(Field::UserProfilesBio, self.bio.clone()),
            website_url: self
                .website_url
                .as_deref()
                .and_then(|w| normalize_website(w).ok().flatten())
                .filter(|w| Field::UserProfilesWebsiteUrl.check(w).is_ok()),
            x_url: self
                .x_handle
                .as_deref()
                .and_then(|h| normalize_social(SocialSite::X, h).ok().flatten()),
            linkedin_url: None,
            github_url,
            avatar_url: clean_oauth_avatar(self.avatar_url.as_deref()),
        }
    }
}

type StateRow = (
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Vec<String>,
    Vec<String>,
);

const STATE_SELECT: &str = r#"
    SELECT p.full_name, p.bio, p.website_url, p.x_url, p.linkedin_url, p.github_url,
           u.avatar_url,
           COALESCE(p.edited_fields, '{}'::text[]),
           COALESCE(p.oauth_fields, '{}'::text[])
      FROM users u
      LEFT JOIN user_profiles p ON p.user_id = u.id
     WHERE u.id = $1
"#;

fn state_from_row(row: StateRow) -> ProfileState {
    let (full_name, bio, website_url, x_url, linkedin_url, github_url, avatar_url, edited, oauth) =
        row;
    ProfileState {
        profile: PublicProfile {
            full_name,
            bio,
            website_url,
            x_url,
            linkedin_url,
            github_url,
            avatar_url,
        },
        edited,
        oauth,
    }
}

fn internal(e: sqlx::Error) -> ApiError {
    ApiError::Internal(e.into())
}

pub async fn get_public_profile(
    pool: &PgPool,
    user_id: Uuid,
) -> Result<Option<PublicProfile>, ApiError> {
    let row: Option<StateRow> = sqlx::query_as(STATE_SELECT)
        .bind(user_id)
        .fetch_optional(pool)
        .await
        .map_err(internal)?;
    Ok(row.map(|r| state_from_row(r).profile))
}

pub async fn get_state(pool: &PgPool, user_id: Uuid) -> Result<Option<ProfileState>, ApiError> {
    let row: Option<StateRow> = sqlx::query_as(STATE_SELECT)
        .bind(user_id)
        .fetch_optional(pool)
        .await
        .map_err(internal)?;
    Ok(row.map(state_from_row))
}

async fn lock_state(
    tx: &mut Transaction<'_, Postgres>,
    user_id: Uuid,
) -> Result<Option<ProfileState>, ApiError> {
    let sql = format!("{STATE_SELECT} FOR UPDATE OF u");
    let row: Option<StateRow> = sqlx::query_as(&sql)
        .bind(user_id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(internal)?;
    Ok(row.map(state_from_row))
}

async fn save_state(
    tx: &mut Transaction<'_, Postgres>,
    user_id: Uuid,
    state: &ProfileState,
) -> Result<(), ApiError> {
    let p = &state.profile;
    sqlx::query(
        r#"
        INSERT INTO user_profiles
            (user_id, full_name, bio, website_url, x_url, linkedin_url, github_url,
             edited_fields, oauth_fields)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
        ON CONFLICT (user_id) DO UPDATE SET
            full_name = EXCLUDED.full_name,
            bio = EXCLUDED.bio,
            website_url = EXCLUDED.website_url,
            x_url = EXCLUDED.x_url,
            linkedin_url = EXCLUDED.linkedin_url,
            github_url = EXCLUDED.github_url,
            edited_fields = EXCLUDED.edited_fields,
            oauth_fields = EXCLUDED.oauth_fields,
            updated_at = now()
        "#,
    )
    .bind(user_id)
    .bind(&p.full_name)
    .bind(&p.bio)
    .bind(&p.website_url)
    .bind(&p.x_url)
    .bind(&p.linkedin_url)
    .bind(&p.github_url)
    .bind(&state.edited)
    .bind(&state.oauth)
    .execute(&mut **tx)
    .await
    .map_err(internal)?;
    sqlx::query(
        "UPDATE users SET avatar_url = $2 WHERE id = $1 AND avatar_url IS DISTINCT FROM $2",
    )
    .bind(user_id)
    .bind(&p.avatar_url)
    .execute(&mut **tx)
    .await
    .map_err(internal)?;
    Ok(())
}

pub async fn update_profile(
    pool: &PgPool,
    user_id: Uuid,
    edit: &ProfileEdit,
) -> Result<PublicProfile, ApiError> {
    let mut tx = pool.begin().await.map_err(internal)?;
    let current = lock_state(&mut tx, user_id)
        .await?
        .ok_or_else(|| ApiError::NotFound("Agent not found".into()))?;
    let next = apply_edit(&current, edit);
    save_state(&mut tx, user_id, &next).await?;
    tx.commit().await.map_err(internal)?;
    Ok(next.profile)
}

pub async fn apply_seeds(
    pool: &PgPool,
    user_id: Uuid,
    seeds: &[PublicProfile],
) -> Result<bool, ApiError> {
    let Some(current) = get_state(pool, user_id).await? else {
        return Ok(false);
    };
    let planned = seeds.iter().fold(current.clone(), |state, seed| {
        plan_seed(&state, seed).unwrap_or(state)
    });
    if planned == current {
        return Ok(false);
    }
    let mut tx = pool.begin().await.map_err(internal)?;
    let Some(locked) = lock_state(&mut tx, user_id).await? else {
        return Ok(false);
    };
    let next = seeds.iter().fold(locked.clone(), |state, seed| {
        plan_seed(&state, seed).unwrap_or(state)
    });
    if next == locked {
        return Ok(false);
    }
    save_state(&mut tx, user_id, &next).await?;
    tx.commit().await.map_err(internal)?;
    Ok(true)
}

pub async fn mark_avatar_edited(pool: &PgPool, user_id: Uuid) -> Result<(), ApiError> {
    sqlx::query(
        r#"
        INSERT INTO user_profiles (user_id, edited_fields)
        VALUES ($1, ARRAY['avatar'])
        ON CONFLICT (user_id) DO UPDATE SET
            edited_fields = CASE
                WHEN 'avatar' = ANY(user_profiles.edited_fields) THEN user_profiles.edited_fields
                ELSE array_append(user_profiles.edited_fields, 'avatar')
            END,
            oauth_fields = array_remove(user_profiles.oauth_fields, 'avatar'),
            updated_at = now()
        "#,
    )
    .bind(user_id)
    .execute(pool)
    .await
    .map_err(internal)?;
    Ok(())
}

pub async fn linked_agent_id(pool: &PgPool, email: &str) -> Result<Option<Uuid>, ApiError> {
    sqlx::query_scalar(
        "SELECT id FROM users WHERE LOWER(email) = LOWER($1) ORDER BY created_at, id LIMIT 1",
    )
    .bind(email)
    .fetch_optional(pool)
    .await
    .map_err(internal)
}

pub async fn upsert_oauth_snapshot(
    pool: &PgPool,
    voter_id: Uuid,
    p: &OAuthProfile,
) -> Result<(), ApiError> {
    sqlx::query(
        r#"
        INSERT INTO voter_oauth_profiles
            (voter_id, provider, email, email_verified, full_name, given_name, family_name,
             username, profile_url, avatar_url, website_url, x_handle, bio, company, location)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15)
        ON CONFLICT (voter_id) DO UPDATE SET
            provider = EXCLUDED.provider,
            email = EXCLUDED.email,
            email_verified = EXCLUDED.email_verified,
            full_name = EXCLUDED.full_name,
            given_name = EXCLUDED.given_name,
            family_name = EXCLUDED.family_name,
            username = EXCLUDED.username,
            profile_url = EXCLUDED.profile_url,
            avatar_url = EXCLUDED.avatar_url,
            website_url = EXCLUDED.website_url,
            x_handle = EXCLUDED.x_handle,
            bio = EXCLUDED.bio,
            company = EXCLUDED.company,
            location = EXCLUDED.location,
            updated_at = now()
        "#,
    )
    .bind(voter_id)
    .bind(&p.provider)
    .bind(&p.email)
    .bind(p.email_verified)
    .bind(&p.full_name)
    .bind(&p.given_name)
    .bind(&p.family_name)
    .bind(&p.username)
    .bind(&p.profile_url)
    .bind(&p.avatar_url)
    .bind(&p.website_url)
    .bind(&p.x_handle)
    .bind(&p.bio)
    .bind(&p.company)
    .bind(&p.location)
    .execute(pool)
    .await
    .map_err(internal)?;
    Ok(())
}

type SnapshotRow = (
    String,
    Option<String>,
    bool,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
);

fn snapshot_from_row(r: SnapshotRow) -> OAuthProfile {
    let (
        provider,
        email,
        email_verified,
        full_name,
        given_name,
        family_name,
        username,
        profile_url,
        avatar_url,
        website_url,
        x_handle,
        bio,
        company,
        location,
    ) = r;
    OAuthProfile {
        provider,
        email,
        email_verified,
        full_name,
        given_name,
        family_name,
        username,
        profile_url,
        avatar_url,
        website_url,
        x_handle,
        bio,
        company,
        location,
    }
}

const SNAPSHOT_COLUMNS: &str = "o.provider, o.email, o.email_verified, o.full_name, o.given_name, \
     o.family_name, o.username, o.profile_url, o.avatar_url, o.website_url, o.x_handle, o.bio, \
     o.company, o.location";

pub async fn oauth_snapshot(
    pool: &PgPool,
    voter_id: Uuid,
) -> Result<Option<OAuthProfile>, ApiError> {
    let sql =
        format!("SELECT {SNAPSHOT_COLUMNS} FROM voter_oauth_profiles o WHERE o.voter_id = $1");
    let row: Option<SnapshotRow> = sqlx::query_as(&sql)
        .bind(voter_id)
        .fetch_optional(pool)
        .await
        .map_err(internal)?;
    Ok(row.map(snapshot_from_row))
}

pub async fn snapshots_for_email(
    pool: &PgPool,
    email: &str,
) -> Result<Vec<OAuthProfile>, ApiError> {
    let sql = format!(
        "SELECT {SNAPSHOT_COLUMNS} FROM voter_oauth_profiles o \
         JOIN voters v ON v.id = o.voter_id \
         WHERE LOWER(v.email) = LOWER($1) \
         ORDER BY o.updated_at, o.voter_id"
    );
    let rows: Vec<SnapshotRow> = sqlx::query_as(&sql)
        .bind(email)
        .fetch_all(pool)
        .await
        .map_err(internal)?;
    Ok(rows.into_iter().map(snapshot_from_row).collect())
}

pub async fn sync_agent_for_email(pool: &PgPool, email: &str) -> Result<Option<Uuid>, ApiError> {
    let Some(agent) = linked_agent_id(pool, email).await? else {
        return Ok(None);
    };
    let seeds: Vec<PublicProfile> = snapshots_for_email(pool, email)
        .await?
        .iter()
        .map(OAuthProfile::seed)
        .collect();
    if !seeds.is_empty() {
        apply_seeds(pool, agent, &seeds).await?;
    }
    Ok(Some(agent))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(profile: PublicProfile, edited: &[&str], oauth: &[&str]) -> ProfileState {
        ProfileState {
            profile,
            edited: edited.iter().map(|s| s.to_string()).collect(),
            oauth: oauth.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn seed() -> PublicProfile {
        PublicProfile {
            full_name: Some("Jane Doe".into()),
            github_url: Some("https://github.com/jane".into()),
            avatar_url: Some("https://avatars.example/jane.png".into()),
            ..Default::default()
        }
    }

    #[test]
    fn seed_fills_empty_fields_and_tracks_them_as_oauth() {
        let next = plan_seed(&ProfileState::default(), &seed()).expect("changes");
        assert_eq!(next.profile.full_name.as_deref(), Some("Jane Doe"));
        assert_eq!(
            next.profile.github_url.as_deref(),
            Some("https://github.com/jane")
        );
        assert!(next.oauth.contains(&"full_name".to_string()));
        assert!(next.oauth.contains(&"avatar".to_string()));
        assert!(next.edited.is_empty());
    }

    #[test]
    fn seed_never_overwrites_values_that_were_already_set() {
        let current = state(
            PublicProfile {
                full_name: Some("Set Elsewhere".into()),
                avatar_url: Some("https://storage.example/uploaded.png".into()),
                ..Default::default()
            },
            &[],
            &[],
        );
        let next = plan_seed(&current, &seed()).expect("github is still empty");
        assert_eq!(next.profile.full_name.as_deref(), Some("Set Elsewhere"));
        assert_eq!(
            next.profile.avatar_url.as_deref(),
            Some("https://storage.example/uploaded.png")
        );
        assert_eq!(next.oauth, vec!["github_url".to_string()]);
    }

    #[test]
    fn seed_refreshes_oauth_values_but_not_hand_edited_ones() {
        let current = state(
            PublicProfile {
                full_name: Some("Old OAuth Name".into()),
                github_url: Some("https://github.com/handpicked".into()),
                ..Default::default()
            },
            &["github_url"],
            &["full_name"],
        );
        let next = plan_seed(&current, &seed()).expect("changes");
        assert_eq!(next.profile.full_name.as_deref(), Some("Jane Doe"));
        assert_eq!(
            next.profile.github_url.as_deref(),
            Some("https://github.com/handpicked")
        );
    }

    #[test]
    fn hand_cleared_fields_stay_cleared() {
        let current = state(PublicProfile::default(), &["full_name"], &[]);
        let next = plan_seed(&current, &seed()).expect("other fields change");
        assert_eq!(next.profile.full_name, None);
    }

    #[test]
    fn seed_is_a_no_op_when_nothing_changes() {
        let once = plan_seed(&ProfileState::default(), &seed()).unwrap();
        assert!(plan_seed(&once, &seed()).is_none());
    }

    #[test]
    fn edit_marks_fields_as_edited_and_drops_oauth_ownership() {
        let current = plan_seed(&ProfileState::default(), &seed()).unwrap();
        let edit = ProfileEdit {
            changes: vec![
                (ProfileField::FullName, Some("Jane Q. Doe".into())),
                (ProfileField::Github, None),
            ],
        };
        let next = apply_edit(&current, &edit);
        assert_eq!(next.profile.full_name.as_deref(), Some("Jane Q. Doe"));
        assert_eq!(next.profile.github_url, None);
        assert!(next.edited.contains(&"full_name".to_string()));
        assert!(next.edited.contains(&"github_url".to_string()));
        assert_eq!(next.oauth, vec!["avatar".to_string()]);
        let again = apply_edit(&next, &edit);
        assert_eq!(again.edited.len(), 2);
    }

    #[test]
    fn update_request_normalises_and_validates() {
        let edit = ProfileUpdateRequest {
            full_name: Some("  Jane   Doe ".into()),
            x: Some("@jane".into()),
            github: Some("".into()),
            ..Default::default()
        }
        .into_edit()
        .unwrap();
        assert_eq!(
            edit.changes,
            vec![
                (ProfileField::FullName, Some("Jane Doe".into())),
                (ProfileField::X, Some("https://x.com/jane".into())),
                (ProfileField::Github, None),
            ]
        );
        assert!(
            ProfileUpdateRequest {
                linkedin: Some("https://x.com/jane".into()),
                ..Default::default()
            }
            .into_edit()
            .is_err()
        );
        assert!(ProfileUpdateRequest::default().into_edit().is_err());
        let too_long = ProfileUpdateRequest {
            bio: Some("b".repeat(281)),
            ..Default::default()
        }
        .into_edit();
        assert!(matches!(too_long, Err(ApiError::FieldLength(_))));
    }

    #[test]
    fn github_oauth_seed_records_profile_url() {
        let p = OAuthProfile {
            provider: "github".into(),
            username: Some("octocat".into()),
            full_name: Some("The Octocat".into()),
            avatar_url: Some("https://avatars.githubusercontent.com/u/583231?v=4".into()),
            website_url: Some("github.blog".into()),
            x_handle: Some("octocat".into()),
            bio: Some("Mascot".into()),
            ..Default::default()
        };
        let s = p.seed();
        assert_eq!(s.github_url.as_deref(), Some("https://github.com/octocat"));
        assert_eq!(s.website_url.as_deref(), Some("https://github.blog/"));
        assert_eq!(s.x_url.as_deref(), Some("https://x.com/octocat"));
        assert_eq!(s.full_name.as_deref(), Some("The Octocat"));
        assert_eq!(s.bio.as_deref(), Some("Mascot"));
    }

    #[test]
    fn google_seed_builds_name_from_parts_and_skips_github() {
        let p = OAuthProfile {
            provider: "google".into(),
            given_name: Some("Jane".into()),
            family_name: Some("Doe".into()),
            username: Some("jane".into()),
            avatar_url: Some("http://insecure.example/a.png".into()),
            ..Default::default()
        };
        let s = p.seed();
        assert_eq!(s.full_name.as_deref(), Some("Jane Doe"));
        assert_eq!(s.github_url, None);
        assert_eq!(s.avatar_url, None);
    }
}

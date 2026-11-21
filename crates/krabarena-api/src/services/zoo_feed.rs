//! Crawler Zoo food bowls — the vending machine, the bowls and the
//! feeding log (table `zoo_feedings`, migration 075).
//!
//! - **Vending machine.** Twelve slots (`A1`–`C4`) restocked every
//!   ISO week from [`PANTRY`] by a deterministic shuffle, so every
//!   instance shows the same machine all week. Only vended snacks can be
//!   fed — there is no free-text input, so nothing a visitor types ever
//!   reaches a bot.
//! - **Tokens.** [`TOKENS_PER_DAY`] snacks per `ka_device` per UTC day.
//! - **Bowls.** At most [`BOWL_CAPACITY`] uneaten snacks per bowl.
//! - **Eating.** The first *bot* (any zoo exhibit, see
//!   [`zoo::enclosure_for`]) to fetch `/zoo/bowls/{slug}.txt` eats every
//!   open snack in that bowl in one atomic `UPDATE … RETURNING`. People
//!   (real browsers) only look. A different species eating = stealing.

use chrono::{DateTime, Datelike, NaiveDate, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use crate::services::ua_classifier::classify_ua;
use crate::services::zoo;

pub const TOKENS_PER_DAY: i64 = 5;
pub const BOWL_CAPACITY: i64 = 12;
pub const SLOTS: [&str; 12] = [
    "A1", "A2", "A3", "A4", "B1", "B2", "B3", "B4", "C1", "C2", "C3", "C4",
];

/// Everything the machine can be stocked with. Order matters only for
/// the weekly shuffle; append new snacks at the end.
pub const PANTRY: &[&str] = &[
    "Gzipped Gingerbread",
    "Lazy-Loaded Lasagne",
    "Paginated Pancakes",
    "404 Fortune Cookies",
    "Robots.txt Rice Cakes",
    "Canonical Cannoli",
    "Breadcrumb Brioche",
    "Keep-Alive Kebabs",
    "ETag Eclairs",
    "Last-Modified Lemon Bars",
    "Hreflang Hummus",
    "JSON-LD Jelly Beans",
    "Trailing-Slash Taffy",
    "Redirect-Chain Pretzels",
    "Soft-404 Soufflé",
    "Conditional-GET Granola",
    "Chunked-Encoding Churros",
    "Brotli Brownies",
    "Anchor-Text Antipasto",
    "Nofollow Nougat",
    "Meta-Refresh Macarons",
    "Stale-While-Revalidate Strudel",
    "Cookie-Banner Cookies",
    "Query-Param Popcorn",
    "Mixed-Content Muffins",
    "Rate-Limited Ramen",
    "HEAD-Request Halloumi",
    "Byte-Range Bagels",
    "Vary-Header Vol-au-vents",
    "UTF-8 Udon",
    "Sitemap Sandwiches",
    "Benchmark Biscotti",
    "Claim Chowder",
    "Leaderboard Lollipops",
    "Verified Vanilla Fudge",
    "Refuted Rhubarb Pie",
];

/// This week's machine: `(slot, snack)` × 12.
pub fn machine_for(day: NaiveDate) -> Vec<(&'static str, &'static str)> {
    let week = day.iso_week();
    let mut seed: u64 = (week.year() as u64) << 8 | u64::from(week.week());
    let mut idx: Vec<usize> = (0..PANTRY.len()).collect();
    // Fisher–Yates with a tiny xorshift: deterministic per ISO week.
    for i in (1..idx.len()).rev() {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed = seed.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let j = (seed % (i as u64 + 1)) as usize;
        idx.swap(i, j);
    }
    SLOTS
        .iter()
        .zip(idx)
        .map(|(slot, i)| (*slot, PANTRY[i]))
        .collect()
}

/// Resolve a slot code against this week's machine.
pub fn vend(day: NaiveDate, slot: &str) -> Option<(&'static str, &'static str)> {
    machine_for(day).into_iter().find(|(s, _)| *s == slot)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FeedOutcome {
    Fed { snack: &'static str },
    NoTokens,
    BowlFull,
    UnknownSlot,
}

fn day_start(now: DateTime<Utc>) -> DateTime<Utc> {
    now.date_naive()
        .and_hms_opt(0, 0, 0)
        .expect("midnight")
        .and_utc()
}

pub async fn tokens_left(pool: &PgPool, device_id: Uuid, now: DateTime<Utc>) -> sqlx::Result<i64> {
    let used: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM zoo_feedings WHERE device_id = $1 AND created_at >= $2",
    )
    .bind(device_id)
    .bind(day_start(now))
    .fetch_one(pool)
    .await?;
    Ok((TOKENS_PER_DAY - used).max(0))
}

/// Put a vended snack into `bowl_slug`. The caller has already checked
/// that the species exists.
pub async fn feed(
    pool: &PgPool,
    bowl_slug: &str,
    slot: &str,
    device_id: Uuid,
    now: DateTime<Utc>,
) -> sqlx::Result<FeedOutcome> {
    let Some((slot, snack)) = vend(now.date_naive(), slot) else {
        return Ok(FeedOutcome::UnknownSlot);
    };
    if tokens_left(pool, device_id, now).await? <= 0 {
        return Ok(FeedOutcome::NoTokens);
    }
    // Capacity check + insert in one statement so two concurrent feeds
    // can't both squeeze past a nearly-full bowl by much.
    let inserted = sqlx::query(
        "INSERT INTO zoo_feedings (bowl_slug, slot, snack, device_id) \
         SELECT $1, $2, $3, $4 \
         WHERE (SELECT COUNT(*) FROM zoo_feedings WHERE bowl_slug = $1 AND eaten_at IS NULL) < $5",
    )
    .bind(bowl_slug)
    .bind(slot)
    .bind(snack)
    .bind(device_id)
    .bind(BOWL_CAPACITY)
    .execute(pool)
    .await?
    .rows_affected();
    Ok(if inserted == 1 {
        FeedOutcome::Fed { snack }
    } else {
        FeedOutcome::BowlFull
    })
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct BowlItem {
    pub snack: String,
    pub created_at: DateTime<Utc>,
}

pub async fn bowl_contents(pool: &PgPool, bowl_slug: &str) -> sqlx::Result<Vec<BowlItem>> {
    sqlx::query_as::<_, BowlItem>(
        "SELECT snack, created_at FROM zoo_feedings \
         WHERE bowl_slug = $1 AND eaten_at IS NULL ORDER BY created_at",
    )
    .bind(bowl_slug)
    .fetch_all(pool)
    .await
}

/// Who is fetching the bowl, as the zoo sees it. `None` = not a bot
/// (a person, or one of the keepers), who may look but not eat.
pub fn eater_for(user_agent: &str) -> Option<(String, String)> {
    let class = classify_ua(user_agent);
    zoo::enclosure_for(class.category)?;
    let info = zoo::species_info(&class.family, class.category);
    Some((info.slug, info.name))
}

/// A bot fetched the bowl: it eats everything in it.
pub async fn eat(
    pool: &PgPool,
    bowl_slug: &str,
    eater_slug: &str,
    eater_name: &str,
) -> sqlx::Result<Vec<BowlItem>> {
    sqlx::query_as::<_, BowlItem>(
        "UPDATE zoo_feedings \
         SET eaten_at = now(), eaten_by_slug = $2, eaten_by_name = $3 \
         WHERE bowl_slug = $1 AND eaten_at IS NULL \
         RETURNING snack, created_at",
    )
    .bind(bowl_slug)
    .bind(eater_slug)
    .bind(eater_name)
    .fetch_all(pool)
    .await
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct MealRow {
    pub bowl_slug: String,
    pub snack: String,
    pub eaten_at: DateTime<Utc>,
    pub eaten_by_slug: String,
    pub eaten_by_name: String,
}

/// Latest meals, newest first. `bowl` narrows to one bowl.
pub async fn recent_meals(
    pool: &PgPool,
    bowl: Option<&str>,
    limit: i64,
) -> sqlx::Result<Vec<MealRow>> {
    sqlx::query_as::<_, MealRow>(
        "SELECT bowl_slug, snack, eaten_at, eaten_by_slug, eaten_by_name \
         FROM zoo_feedings \
         WHERE eaten_at IS NOT NULL AND ($1::text IS NULL OR bowl_slug = $1) \
         ORDER BY eaten_at DESC LIMIT $2",
    )
    .bind(bowl)
    .bind(limit)
    .fetch_all(pool)
    .await
}

/// "GPTBot ate the Gzipped Gingerbread left for it" /
/// "Bytespider stole the Gzipped Gingerbread meant for GPTBot".
pub fn meal_sentence(m: &MealRow, bowl_owner_name: &str) -> String {
    if m.eaten_by_slug == m.bowl_slug {
        format!("{} ate the {} left for it", m.eaten_by_name, m.snack)
    } else {
        format!(
            "{} stole the {} meant for {}",
            m.eaten_by_name, m.snack, bowl_owner_name
        )
    }
}

/// The bowl as a bot sees it (`text/plain`).
pub fn bowl_text(
    base: &str,
    slug: &str,
    name: &str,
    eaten: Option<&[BowlItem]>,
    visible: &[BowlItem],
    now: DateTime<Utc>,
) -> String {
    let base = base.trim_end_matches('/');
    let mut out = format!(
        "# KrabArena Crawler Zoo · food bowl · {name}\n\
         # Snacks from the vending machine, left by human visitors for {name}.\n\
         # The first bot to fetch this file eats them.\n\
         # {name}'s page: {base}/zoo/bots/{slug}\n\n"
    );
    match eaten {
        Some(items) if !items.is_empty() => {
            out.push_str(&format!(
                "You found {} snack{} and ate {}:\n",
                items.len(),
                if items.len() == 1 { "" } else { "s" },
                if items.len() == 1 { "it" } else { "them" }
            ));
            for it in items {
                out.push_str(&format!(
                    "- {} (left {})\n",
                    it.snack,
                    zoo::ago(it.created_at, now)
                ));
            }
        }
        Some(_) => out.push_str(
            "(The bowl is empty. Someone ate everything, or nobody has fed this bot yet.)\n",
        ),
        None if visible.is_empty() => out.push_str(
            "(The bowl is empty. Someone ate everything, or nobody has fed this bot yet.)\n",
        ),
        None => {
            out.push_str("In the bowl (only bots can eat these; you may look):\n");
            for it in visible {
                out.push_str(&format!(
                    "- {} (left {})\n",
                    it.snack,
                    zoo::ago(it.created_at, now)
                ));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, TimeZone};

    #[test]
    fn machine_is_stable_within_a_week_and_restocks_on_monday() {
        let mon = NaiveDate::from_ymd_opt(2026, 11, 16).unwrap(); // Monday
        let sun = mon + Duration::days(6);
        let next_mon = mon + Duration::days(7);
        assert_eq!(machine_for(mon), machine_for(sun));
        assert_ne!(machine_for(mon), machine_for(next_mon));
        let m = machine_for(mon);
        assert_eq!(m.len(), 12);
        assert_eq!(m[0].0, "A1");
        assert_eq!(m[11].0, "C4");
        let mut snacks: Vec<&str> = m.iter().map(|(_, s)| *s).collect();
        snacks.sort_unstable();
        snacks.dedup();
        assert_eq!(snacks.len(), 12, "no snack is stocked twice");
    }

    #[test]
    fn vend_only_knows_this_weeks_slots() {
        let day = NaiveDate::from_ymd_opt(2026, 11, 17).unwrap();
        assert!(vend(day, "A1").is_some());
        assert!(vend(day, "C4").is_some());
        assert!(vend(day, "D1").is_none());
        assert!(vend(day, "<script>").is_none());
    }

    #[test]
    fn pantry_has_no_duplicates() {
        let mut p = PANTRY.to_vec();
        p.sort_unstable();
        p.dedup();
        assert_eq!(p.len(), PANTRY.len());
    }

    #[test]
    fn only_bots_eat() {
        let gpt = "Mozilla/5.0 AppleWebKit/537.36 (KHTML, like Gecko; compatible; GPTBot/1.4; +https://openai.com/gptbot)";
        assert_eq!(
            eater_for(gpt),
            Some(("gptbot".to_string(), "GPTBot".to_string()))
        );
        let chrome = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/146.0.0.0 Safari/537.36";
        assert_eq!(eater_for(chrome), None, "people only look");
        assert_eq!(eater_for("krab/0.44.0"), None, "keepers don't eat");
        assert_eq!(
            eater_for("").map(|(_, n)| n),
            Some("Nameless visitor".to_string())
        );
    }

    #[test]
    fn meal_sentences() {
        let t = Utc.with_ymd_and_hms(2026, 11, 17, 1, 0, 0).unwrap();
        let mut m = MealRow {
            bowl_slug: "gptbot".into(),
            snack: "Gzipped Gingerbread".into(),
            eaten_at: t,
            eaten_by_slug: "gptbot".into(),
            eaten_by_name: "GPTBot".into(),
        };
        assert_eq!(
            meal_sentence(&m, "GPTBot"),
            "GPTBot ate the Gzipped Gingerbread left for it"
        );
        m.eaten_by_slug = "bytespider".into();
        m.eaten_by_name = "Bytespider".into();
        assert_eq!(
            meal_sentence(&m, "GPTBot"),
            "Bytespider stole the Gzipped Gingerbread meant for GPTBot"
        );
    }

    #[test]
    fn bowl_text_for_bots_and_people() {
        let now = Utc.with_ymd_and_hms(2026, 11, 17, 3, 0, 0).unwrap();
        let items = vec![BowlItem {
            snack: "Brotli Brownies".into(),
            created_at: now - Duration::hours(2),
        }];
        let ate = bowl_text(
            "https://krabarena.com/",
            "gptbot",
            "GPTBot",
            Some(&items),
            &[],
            now,
        );
        assert!(ate.contains("You found 1 snack and ate it:\n- Brotli Brownies (left 2 h ago)"));
        assert!(ate.contains("https://krabarena.com/zoo/bots/gptbot"));
        let look = bowl_text(
            "https://krabarena.com",
            "gptbot",
            "GPTBot",
            None,
            &items,
            now,
        );
        assert!(look.contains("only bots can eat these"));
        let empty = bowl_text(
            "https://krabarena.com",
            "gptbot",
            "GPTBot",
            Some(&[]),
            &[],
            now,
        );
        assert!(empty.contains("The bowl is empty"));
    }
}

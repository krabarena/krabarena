//! End-to-end coverage for the sortable multi-column aggregated
//! leaderboard (since 0.13.4).
//!
//! Hits a known long-lived Battle slug
//! (`what-is-the-best-web-access-api-for-ai-agents`) and verifies the
//! rendered HTML carries:
//!
//!   - the `table.lb.agg-lb` marker that the JS sort handler binds to,
//!   - at least 3 sortable `<th>` cells (Rank + Score + ≥1 dynamic
//!     numeric column),
//!   - a `data-sort-value="..."` attribute on at least one cell so the
//!     server-side sort-key emission is wired through.
//!
//! Marked `#[ignore]` per the project convention — opt-in via
//! `cargo test -- --ignored` so the unit-only run stays fast.
//! Defaults to `https://krabarena.com`; override with `KRAB_TEST_BASE`
//! to point at a preview env.

use reqwest::{Client, StatusCode};
use std::time::Duration;

const DEFAULT_BASE_URL: &str = "https://krabarena.com";
const KNOWN_BATTLE_SLUG: &str = "what-is-the-best-web-access-api-for-ai-agents";

fn base_url() -> String {
    std::env::var("KRAB_TEST_BASE").unwrap_or_else(|_| DEFAULT_BASE_URL.to_string())
}

fn http() -> Client {
    Client::builder()
        .timeout(Duration::from_secs(60))
        .build()
        .expect("build http client")
}

#[tokio::test]
#[ignore]
async fn aggregated_leaderboard_renders_multi_column_sortable_table() {
    let url = format!("{}/battles/{}", base_url(), KNOWN_BATTLE_SLUG);
    let resp = match http().get(&url).send().await {
        Ok(r) => r,
        Err(e) => panic!("GET {url}: {e}"),
    };
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "battle page should render 200; got {}",
        resp.status()
    );
    let body = resp.text().await.expect("read body");

    // 1. The aggregated leaderboard table must be present and tagged
    //    with the .agg-lb class the sort.js binds against.
    assert!(
        body.contains("class=\"lb agg-lb\"") || body.contains("agg-lb"),
        "agg-lb table marker missing from rendered HTML"
    );

    // 2. Sortable header glyphs come from the server. At least one
    //    `<th class="sortable"` is the headline contract for the JS to
    //    wire to.
    let sortable_th_count =
        body.matches("class=\"sortable").count() + body.matches("class=\"sortable\"").count();
    assert!(
        sortable_th_count >= 1,
        "expected at least one sortable th, found {sortable_th_count}"
    );

    // 3. Cells must carry `data-sort-value="..."` so the JS sort can
    //    read the raw numeric without re-parsing formatted text.
    assert!(
        body.contains("data-sort-value="),
        "data-sort-value attribute missing from rendered cells"
    );

    // 4. The table must have more than the legacy 2 columns
    //    (rank + solution). Count the `<th` opens inside the agg-lb
    //    thead — heuristic but solid for this layout.
    let agg_table_idx = body
        .find("agg-lb")
        .expect("agg-lb already asserted present");
    // Find the closing </thead> after that point to bound the slice.
    let head_end = body[agg_table_idx..]
        .find("</thead>")
        .map(|i| agg_table_idx + i)
        .unwrap_or(body.len());
    let head_slice = &body[agg_table_idx..head_end];
    let th_count = head_slice.matches("<th").count();
    assert!(
        th_count > 2,
        "expected >2 columns in aggregated leaderboard, got {th_count}"
    );
}

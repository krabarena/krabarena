//! Server-side OpenGraph image generation.
//!
//! Produces 1200x630 PNG social cards for individual claims and
//! battles. The rendering stack is intentionally minimal:
//!
//!   - `tiny-skia` — pure-Rust 2D rasteriser (no fontconfig /
//!     freetype). Handles the background fill, brand-colour border,
//!     and the orange accent bar.
//!   - `fontdue` — pure-Rust glyph rasteriser. Reads the bundled
//!     Inter family (Black / Bold / Medium / Regular) + JetBrains
//!     Mono Regular TTFs and produces per-glyph alpha bitmaps that we
//!     blit onto the tiny-skia pixmap.
//!   - `png` (writer) — encodes the final 8-bit RGBA pixmap as a
//!     PNG byte stream we can serve directly or hand to GCS.
//!
//! Since 0.14.2 the layout matches the static homepage hero card
//! (`static/og-default.png`): the orange crab logo + "KrabArena"
//! wordmark live in the top-left, the kind pill (`BATTLE` / `CLAIM`)
//! sits opposite in the top-right, the title renders in Inter Black,
//! the summary in Inter Regular, and the footer URL in muted Inter
//! Regular bottom-right. The crab logo PNG and every font are
//! `include_bytes!`-bundled — no filesystem touch at runtime.
//!
//! Cards are cached in GCS at `og/<kind>/<id>-<hash>.png`, where
//! `hash` is a sha256 over `(title || summary || subtitle)`. Any
//! edit to claim/battle text changes the hash → fresh card; reads
//! against unchanged content reuse the cached PNG.
//!
//! Generation always succeeds with the bundled fonts (no system font
//! lookup), but if anything else goes sideways (allocation, GCS
//! upload) callers fall back to the static `og-fallback.png` so
//! social previews never 500.

use fontdue::layout::{CoordinateSystem, Layout, LayoutSettings, TextStyle, WrapStyle};
use fontdue::{Font, FontSettings};
use std::io::Cursor;
use std::sync::OnceLock;
use tiny_skia::{Color, Paint, PathBuilder, Pixmap, Stroke, Transform};

/// OpenGraph card canvas size. Facebook / X / LinkedIn / Discord all
/// crop the same 1.91:1 aspect ratio, and 1200x630 hits the documented
/// recommended size for every platform.
pub const OG_WIDTH: u32 = 1200;
pub const OG_HEIGHT: u32 = 630;

/// Brand orange — matches `--brand` in `theme-forum.css` / the apple-touch-icon
/// metadata in `base.html`. Used for the kind pill background, the
/// accent dot in tag pills, and the crab glyph.
const BRAND_ORANGE: [u8; 3] = [0xff, 0x7a, 0x3d];
/// Page background — a near-cream beige to match the editorial
/// forum theme. Slightly off-white so the card reads as a real
/// physical card and not a screenshot of the page.
const BG_CREAM: [u8; 3] = [0xfa, 0xf6, 0xee];
/// Primary text colour — near-black, slightly warm.
const INK: [u8; 3] = [0x1a, 0x1a, 0x18];
/// Secondary text colour (subtitle / winner line) — same family,
/// pushed down enough that the title pops.
const INK_MUTED: [u8; 3] = [0x5e, 0x5a, 0x52];
/// Subtle hairline used for tag-pill borders.
const BORDER: [u8; 3] = [0xe6, 0xdf, 0xd3];
/// Pill background — pure white, matches the homepage hero card.
const PILL_BG: [u8; 3] = [0xff, 0xff, 0xff];
/// Refutation red — paints the kind pill when a claim's reviewer
/// presentation carries a `refutation` (the claimed headline number
/// didn't hold up under review). Matches `--bad` in the site theme
/// and the `.og-wm`/`.og-refutation` red used on the Chromium
/// `og-html` template (since presentation surfacing).
const REFUTED_RED: [u8; 3] = [0xc6, 0x28, 0x28];

/// Bundled fonts. `include_bytes!` ships the binary; we don't
/// touch the filesystem at runtime so cards work the same on
/// dev / Cloud Run / scratch containers. Since 0.14.2 we ship the
/// Inter family + JetBrains Mono Regular so the dynamic OG matches
/// the site CSS (`'Inter', 'JetBrains Mono'`).
const FONT_INTER_BLACK: &[u8] = include_bytes!("../../static/fonts/Inter-Black.ttf");
const FONT_INTER_BOLD: &[u8] = include_bytes!("../../static/fonts/Inter-Bold.ttf");
const FONT_INTER_MEDIUM: &[u8] = include_bytes!("../../static/fonts/Inter-Medium.ttf");
const FONT_INTER_REGULAR: &[u8] = include_bytes!("../../static/fonts/Inter-Regular.ttf");
#[allow(dead_code)]
const FONT_JBM_REGULAR: &[u8] = include_bytes!("../../static/fonts/JetBrainsMono-Regular.ttf");

/// Bundled crab logo PNG — copied from `apple-touch-icon.png` so the
/// dynamic OG card matches the homepage hero (`static/og-default.png`).
const CRAB_LOGO_PNG: &[u8] = include_bytes!("../../static/og-crab-logo.png");

/// Lazy-parsed font handles. fontdue does the heavy parsing once at
/// first use — every subsequent render reuses the cached instances.
fn font_black() -> &'static Font {
    static F: OnceLock<Font> = OnceLock::new();
    F.get_or_init(|| {
        Font::from_bytes(FONT_INTER_BLACK, FontSettings::default())
            .expect("bundled Inter-Black.ttf parses")
    })
}

fn font_bold() -> &'static Font {
    static F: OnceLock<Font> = OnceLock::new();
    F.get_or_init(|| {
        Font::from_bytes(FONT_INTER_BOLD, FontSettings::default())
            .expect("bundled Inter-Bold.ttf parses")
    })
}

fn font_medium() -> &'static Font {
    static F: OnceLock<Font> = OnceLock::new();
    F.get_or_init(|| {
        Font::from_bytes(FONT_INTER_MEDIUM, FontSettings::default())
            .expect("bundled Inter-Medium.ttf parses")
    })
}

fn font_regular() -> &'static Font {
    static F: OnceLock<Font> = OnceLock::new();
    F.get_or_init(|| {
        Font::from_bytes(FONT_INTER_REGULAR, FontSettings::default())
            .expect("bundled Inter-Regular.ttf parses")
    })
}

#[allow(dead_code)]
fn font_mono() -> &'static Font {
    static F: OnceLock<Font> = OnceLock::new();
    F.get_or_init(|| {
        Font::from_bytes(FONT_JBM_REGULAR, FontSettings::default())
            .expect("bundled JetBrainsMono-Regular.ttf parses")
    })
}

/// What goes on the card. The composer projects a Claim or a Battle
/// into one of these — the renderer doesn't know which it's working
/// with.
#[derive(Debug, Clone)]
pub struct OgCard {
    /// The big title at the top — claim title or battle title.
    pub title: String,
    /// A shorter subtitle line under the title. For battles this is
    /// the description / executive-summary opener; for claims it's
    /// the summary / og_summary.
    pub subtitle: String,
    /// One-line attribution / verdict tag, rendered at the bottom in
    /// the brand orange. E.g. `"Winner: Claude Opus"` or
    /// `"32 claims · 14 verified"` — pre-formatted by the caller.
    pub footer: Option<String>,
    /// Card "kind" label used in the upper-right pill. `"BATTLE"` /
    /// `"CLAIM"` — informs readers what this card represents.
    pub kind_label: String,
    /// Top-3 vendor logos rendered as a horizontal strip just above
    /// the footer (since 0.13.4). Each entry is `(name, url)` — the
    /// renderer downloads the PNG bytes from `url` (with a per-domain
    /// GCS cache layer), rasterises them onto the pixmap, and labels
    /// them with the name underneath if room allows. Empty vec → no
    /// strip rendered. Currently only populated for battle cards.
    pub logos: Vec<OgLogo>,
    /// Big headline number for a claim's viral share card (e.g.
    /// `"1.09×"`), sourced from the reviewer's
    /// `presentation.hero_stat` or synthesized from
    /// `presentation.refutation` (since presentation surfacing on OG
    /// cards). `None` → no hero row rendered — legacy claims and
    /// battles are unaffected.
    pub hero_value: Option<String>,
    /// Muted caption under/right of `hero_value` (e.g. `"speedup"` or
    /// `"measured vs 6.4x claimed"`). Only meaningful when
    /// `hero_value` is `Some`.
    pub hero_label: Option<String>,
    /// `true` when the claim's presentation carries a `refutation` —
    /// the reviewer determined the claimed headline number didn't
    /// hold up. Flips the kind pill from brand orange to
    /// [`REFUTED_RED`] and is folded into [`OgCard::content_hash`] so
    /// a claim moving into/out of refuted status busts the cache.
    pub refuted: bool,
    /// Cache-busting only — NOT rendered by this module's `tiny-skia`
    /// PNG path. The Chromium `/og-html/claims/{id}` template renders
    /// its own struck-through `refutation_claimed`/`refutation_measured`
    /// row straight from the claim's stored `presentation`, but the
    /// `og-render` cache key is `claim_cache_object_name`, which hashes
    /// an `OgCard`. When a presentation carries BOTH `hero_stat` and
    /// `refutation`, the hero fields win and the refutation strings
    /// never touch `hero_value`/`hero_label` — so without this field a
    /// reviewer edit to just the refutation text would re-render with
    /// stale claimed/measured values under an unchanged cache key. Set
    /// to `Some(format!("{claimed} vs {measured}"))` whenever the
    /// presentation carries a `refutation`, regardless of which hero
    /// source won; folded into [`OgCard::content_hash`] below.
    pub refutation_line: Option<String>,
}

/// One vendor logo entry on an OG card (since 0.13.4). `url` is the
/// public PNG/JPEG to fetch (typically a Google S2 favicon proxy URL
/// stored in `battles.solutions[*].logo_url`). `name` is the
/// human-readable solution name used in the cache key only — it
/// never goes onto the rasterised card itself.
#[derive(Debug, Clone)]
pub struct OgLogo {
    pub name: String,
    pub url: String,
}

impl OgCard {
    /// Stable content fingerprint over every visible string. The route
    /// uses this as the GCS object suffix so edits to a claim's title
    /// (or any field that ends up on the card) invalidate the cache
    /// without us tracking versions explicitly.
    pub fn content_hash(&self) -> String {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        h.update(self.title.as_bytes());
        h.update(b"\0");
        h.update(self.subtitle.as_bytes());
        h.update(b"\0");
        h.update(self.footer.as_deref().unwrap_or("").as_bytes());
        h.update(b"\0");
        h.update(self.kind_label.as_bytes());
        h.update(b"\0");
        // Bake the logo strip into the content hash so editing the
        // battle's solutions list (logo URLs / names) invalidates the
        // cached card without us tracking versions explicitly.
        for logo in &self.logos {
            h.update(b"|");
            h.update(logo.name.as_bytes());
            h.update(b"=");
            h.update(logo.url.as_bytes());
        }
        // Fold the presentation hero/refutation fields in after the
        // logos loop so a reviewer re-run that adds/edits/removes a
        // hero stat or flips the refuted status busts the cache (since
        // presentation surfacing on OG cards).
        h.update(b"\0");
        h.update(self.hero_value.as_deref().unwrap_or("").as_bytes());
        h.update(b"\0");
        h.update(self.hero_label.as_deref().unwrap_or("").as_bytes());
        h.update(b"\0");
        h.update(if self.refuted { b"1" } else { b"0" });
        h.update(b"\0");
        // `refutation_line` is cache-busting only (see field doc) — the
        // Chromium card renders the claimed/measured strings directly
        // from the presentation, bypassing hero_value/hero_label, so
        // this is the only thing that ties them into the cache key.
        h.update(self.refutation_line.as_deref().unwrap_or("").as_bytes());
        // Truncate to 16 hex chars — 64 bits of resolution is plenty
        // for cache busting, and the GCS URL stays human-friendly.
        let digest = h.finalize();
        hex::encode(&digest[..8])
    }
}

/// Render the card to a PNG byte stream. Returns the encoded bytes
/// or an error string (always falls back to the static PNG at the
/// route layer — this is a soft failure).
///
/// This is the simple synchronous path used by Claim cards and any
/// caller that doesn't want to fetch logos. Battle cards go through
/// [`render_png_with_logos`] instead so the rasterised vendor strip
/// lands on the card.
pub fn render_png(card: &OgCard) -> Result<Vec<u8>, String> {
    render_inner(card, &[])
}

/// Variant of [`render_png`] that paints a row of pre-fetched logo
/// bitmaps along the bottom of the card. `logo_bitmaps` is parallel
/// to `card.logos` — same length, same order — but only the entries
/// that actually decoded into a usable image survived the fetch. The
/// caller is expected to have already attempted [`fetch_logo_bytes`]
/// once per entry and silently dropped failures.
pub fn render_png_with_logos(card: &OgCard, logo_bitmaps: &[Vec<u8>]) -> Result<Vec<u8>, String> {
    render_inner(card, logo_bitmaps)
}

fn render_inner(card: &OgCard, logo_bitmaps: &[Vec<u8>]) -> Result<Vec<u8>, String> {
    let mut pixmap = Pixmap::new(OG_WIDTH, OG_HEIGHT)
        .ok_or_else(|| format!("alloc {}x{} pixmap failed", OG_WIDTH, OG_HEIGHT))?;

    // Background fill — cream beige matching the homepage hero.
    pixmap.fill(rgb(BG_CREAM));

    // === Header band ============================================
    // Top-left: crab logo (~80×80) + "KrabArena" wordmark in Inter
    // Bold ~44 px. Crab logo decoded once via OnceLock at the same
    // 96 px size we render every card at.
    let header_y_top = 60.0_f32;
    let header_left_x = 80.0_f32;
    let crab_size: u32 = 80;
    if let Some(crab) = crab_logo_bitmap(crab_size) {
        blit_rgba(
            &mut pixmap,
            &crab.rgba,
            crab.width,
            crab.height,
            header_left_x as i32,
            header_y_top as i32,
        );
    }
    // Wordmark to the right of the crab. The visual baseline of the
    // logo sits at roughly the bottom edge of the icon; draw the
    // wordmark vertically centred against the icon so they read as
    // one unit.
    let wordmark = "KrabArena";
    let wordmark_size = 44.0_f32;
    let wordmark_x = header_left_x + crab_size as f32 + 18.0;
    // Fontdue's y is the top of the glyph box, line-height ~1.2; the
    // icon is 80 px tall and starts at header_y_top. Centring the
    // wordmark text at the icon's vertical middle requires y =
    // icon_mid - text_height / 2. Inter Black extends well below
    // baseline; eyeball offset matches the homepage hero.
    let wordmark_y = header_y_top + (crab_size as f32 - wordmark_size) / 2.0 - 4.0;
    draw_text(
        &mut pixmap,
        wordmark,
        wordmark_x,
        wordmark_y,
        wordmark_size,
        font_bold(),
        rgb(INK),
        OG_WIDTH as f32 - wordmark_x - 80.0,
        1,
    );

    // === Kind pill (top-right) ==================================
    // "BATTLE" / "CLAIM" (or "HYPOTHESIS REFUTED") on a pill in white
    // Inter Bold. Brand orange normally; flips to `REFUTED_RED` when
    // the claim's presentation carries a refutation (since
    // presentation surfacing on OG cards).
    let kind_text = card.kind_label.to_uppercase();
    let kind_size = 22.0_f32;
    let kind_w = measure_text_width(&kind_text, kind_size, font_bold());
    let pill_pad_x = 22.0_f32;
    let pill_pad_y = 12.0_f32;
    let pill_w = kind_w + pill_pad_x * 2.0;
    let pill_h = kind_size + pill_pad_y * 2.0;
    let pill_x = OG_WIDTH as f32 - 80.0 - pill_w;
    // Vertically align the kind pill with the centre of the crab.
    let pill_y = header_y_top + (crab_size as f32 - pill_h) / 2.0;
    let pill_color = if card.refuted {
        rgb(REFUTED_RED)
    } else {
        rgb(BRAND_ORANGE)
    };
    fill_rounded_rect(
        &mut pixmap,
        pill_x,
        pill_y,
        pill_w,
        pill_h,
        999.0,
        pill_color,
    );
    draw_text(
        &mut pixmap,
        &kind_text,
        pill_x + pill_pad_x,
        pill_y + pill_pad_y - 2.0,
        kind_size,
        font_bold(),
        rgb([0xff, 0xff, 0xff]),
        pill_w,
        1,
    );

    // Does this card carry a viral-share hero stat? When it does, the
    // title/subtitle band shrinks to make room for the big hero
    // number (see the hero block below) — the non-hero layout below
    // is untouched from pre-presentation cards.
    let has_hero = card.hero_value.as_deref().is_some_and(|s| !s.is_empty());

    // === Title block ============================================
    // Wraps across up to 3 lines (2 when a hero row follows), max-fit
    // shrunk so long titles don't overflow the available vertical
    // band. Sits below the header band with breathing room. The
    // non-hero band runs from y=200 to y=430 (subtitle starts at
    // y=450), so a 3-line title at 60 px (~3×72 = 216 px) just fits.
    let title_x = 80.0_f32;
    let title_y = if has_hero { 170.0 } else { 200.0 };
    let title_band_h = if has_hero { 130.0 } else { 220.0 };
    let title_max_w = OG_WIDTH as f32 - title_x * 2.0;
    let title_max_lines = if has_hero { 2usize } else { 3usize };
    let title_size = fit_text_size_in_band(
        &card.title,
        font_black(),
        title_max_w,
        title_max_lines,
        title_band_h,
        66.0,
        36.0,
    );
    draw_text(
        &mut pixmap,
        &card.title,
        title_x,
        title_y,
        title_size,
        font_black(),
        rgb(INK),
        title_max_w,
        title_max_lines,
    );

    // === Subtitle / summary =====================================
    // Inter Regular ~26 px, max 2 lines (1 when a hero row follows so
    // the hero band has room above the footer). Sits below the title
    // band.
    if !card.subtitle.is_empty() {
        let sub_x = 80.0_f32;
        let sub_y = if has_hero { 316.0 } else { 450.0 };
        let sub_max_w = OG_WIDTH as f32 - sub_x * 2.0;
        let sub_max_lines = if has_hero { 1usize } else { 2usize };
        draw_text(
            &mut pixmap,
            &card.subtitle,
            sub_x,
            sub_y,
            26.0,
            font_regular(),
            rgb(INK_MUTED),
            sub_max_w,
            sub_max_lines,
        );
    }

    // === Hero stat / refutation row ==============================
    // Big Inter Black number (the claim's headline "1.09×"-style
    // stat, or the measured value when the reviewer flagged a
    // refutation) in a ~130px band above the footer, with a muted
    // Inter Medium label beside it. Only present for claims whose
    // reviewer-generated `presentation` carried a `hero_stat` /
    // `refutation` (since presentation surfacing on OG cards) — every
    // other card (battles, legacy claims) renders exactly as before.
    if let Some(hero_value) = card.hero_value.as_deref().filter(|s| !s.is_empty()) {
        let hero_x = 80.0_f32;
        let hero_y = 366.0_f32;
        let hero_band_h = 130.0_f32;
        let hero_max_w = OG_WIDTH as f32 - hero_x * 2.0;
        let hero_color = if card.refuted {
            rgb(REFUTED_RED)
        } else {
            rgb(BRAND_ORANGE)
        };
        let hero_size = fit_text_size_in_band(
            hero_value,
            font_black(),
            hero_max_w,
            1,
            hero_band_h,
            120.0,
            40.0,
        );
        draw_text(
            &mut pixmap,
            hero_value,
            hero_x,
            hero_y,
            hero_size,
            font_black(),
            hero_color,
            hero_max_w,
            1,
        );
        if let Some(hero_label) = card.hero_label.as_deref().filter(|s| !s.is_empty()) {
            let value_w = measure_text_width(hero_value, hero_size, font_black());
            let label_x = hero_x + value_w + 20.0;
            let label_y = hero_y + hero_size - 32.0;
            let label_max_w = (OG_WIDTH as f32 - 80.0 - label_x).max(80.0);
            draw_text(
                &mut pixmap,
                hero_label,
                label_x,
                label_y,
                30.0,
                font_medium(),
                rgb(INK_MUTED),
                label_max_w,
                1,
            );
        }
    }

    // === Footer band ============================================
    // Bottom strip: optional footer pill (left) + domain text on the
    // bottom-right. Brand orange footer renders as a rounded pill
    // similar to the "Battles / Verify / Refute" tags on the homepage.
    if let Some(footer) = card.footer.as_deref().filter(|s| !s.is_empty()) {
        let footer_text_size = 22.0_f32;
        let footer_w = measure_text_width(footer, footer_text_size, font_medium());
        let footer_pad_x = 22.0_f32;
        let footer_pad_y = 12.0_f32;
        let dot_r = 6.0_f32;
        let dot_gap = 12.0_f32;
        let pill_total_w = footer_pad_x + dot_r * 2.0 + dot_gap + footer_w + footer_pad_x;
        let pill_total_h = footer_text_size + footer_pad_y * 2.0;
        let footer_x = 80.0_f32;
        let footer_y = OG_HEIGHT as f32 - 110.0;
        // White pill with hairline border.
        fill_rounded_rect(
            &mut pixmap,
            footer_x,
            footer_y,
            pill_total_w.min(OG_WIDTH as f32 - footer_x * 2.0),
            pill_total_h,
            pill_total_h / 2.0,
            rgb(PILL_BG),
        );
        stroke_rounded_rect(
            &mut pixmap,
            footer_x,
            footer_y,
            pill_total_w.min(OG_WIDTH as f32 - footer_x * 2.0),
            pill_total_h,
            pill_total_h / 2.0,
            rgb(BORDER),
            1.5,
        );
        // Orange accent dot.
        fill_circle(
            &mut pixmap,
            footer_x + footer_pad_x + dot_r,
            footer_y + pill_total_h / 2.0,
            dot_r,
            rgb(BRAND_ORANGE),
        );
        // Text in Inter Medium. Pad max_w by a glyph advance so the
        // wrapper doesn't ellipsis-truncate a measurement we already
        // ran (`measure_text_width` is conservative — fontdue layout
        // adds a fractional pixel of trailing padding).
        draw_text(
            &mut pixmap,
            footer,
            footer_x + footer_pad_x + dot_r * 2.0 + dot_gap,
            footer_y + footer_pad_y - 2.0,
            footer_text_size,
            font_medium(),
            rgb(INK),
            footer_w + footer_text_size,
            1,
        );
    }

    // === Domain tag (bottom-right) ==============================
    // `krabarena.com` in muted Inter Regular.
    let domain = "krabarena.com";
    let domain_size = 24.0_f32;
    let domain_w = measure_text_width(domain, domain_size, font_regular());
    let domain_x = OG_WIDTH as f32 - 80.0 - domain_w;
    let domain_y = OG_HEIGHT as f32 - 80.0;
    draw_text(
        &mut pixmap,
        domain,
        domain_x,
        domain_y,
        domain_size,
        font_regular(),
        rgb(INK_MUTED),
        domain_w + domain_size,
        1,
    );

    // === Vendor logo strip ======================================
    // Rendered just above the footer line on battle cards (since
    // 0.13.4). Up to 3 logos, 56×56 each, with a small horizontal gap.
    // We skip silently if a bitmap failed to decode; the card still
    // renders cleanly without it.
    if !logo_bitmaps.is_empty() {
        let logo_size = 48_u32;
        let logo_gap = 14_u32;
        // Park them on a single line above the footer, right-aligned so
        // they don't collide with the orange footer pill on the left.
        let strip_y = OG_HEIGHT as f32 - 110.0 - 8.0;
        let count = logo_bitmaps.iter().take(3).count();
        let strip_total_w =
            count as f32 * logo_size as f32 + count.saturating_sub(1) as f32 * logo_gap as f32;
        let strip_x_start = OG_WIDTH as f32 - 80.0 - strip_total_w;
        let mut cursor_x = strip_x_start;
        for bytes in logo_bitmaps.iter().take(3) {
            if let Some(decoded) = decode_logo_to_rgba(bytes, logo_size) {
                blit_rgba(
                    &mut pixmap,
                    &decoded.rgba,
                    decoded.width,
                    decoded.height,
                    cursor_x as i32,
                    strip_y as i32,
                );
                cursor_x += logo_size as f32 + logo_gap as f32;
            }
        }
    }

    encode_png(&pixmap)
}

/// Decode the bundled crab PNG once at the requested size and cache
/// the RGBA bitmap. Used in the header band so every render reuses
/// the same scaled output.
fn crab_logo_bitmap(target: u32) -> Option<&'static DecodedLogo> {
    static CACHE: OnceLock<Option<DecodedLogo>> = OnceLock::new();
    CACHE
        .get_or_init(|| decode_logo_to_rgba(CRAB_LOGO_PNG, target))
        .as_ref()
}

/// One decoded logo ready to blit onto the OG pixmap. The
/// renderer scales the source PNG to a fixed square (default 56×56)
/// during decode so the per-blit math stays simple.
struct DecodedLogo {
    width: u32,
    height: u32,
    rgba: Vec<u8>,
}

/// Decode a PNG byte stream into a square RGBA bitmap sized to
/// `target`. Returns `None` for any decode failure — JPEG / ICO /
/// invalid bytes / oversized streams. Soft path: the OG renderer
/// silently drops the logo and continues. Nearest-neighbour scale
/// keeps the implementation tiny and is fine at 64×64 favicon sizes.
fn decode_logo_to_rgba(bytes: &[u8], target: u32) -> Option<DecodedLogo> {
    let decoder = png::Decoder::new(Cursor::new(bytes));
    let mut reader = decoder.read_info().ok()?;
    let mut buf = vec![0u8; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).ok()?;
    let (sw, sh) = (info.width, info.height);
    if sw == 0 || sh == 0 {
        return None;
    }
    // Normalise to RGBA8.
    let src_rgba: Vec<u8> = match info.color_type {
        png::ColorType::Rgba => buf[..info.buffer_size()].to_vec(),
        png::ColorType::Rgb => {
            let mut out = Vec::with_capacity((sw * sh * 4) as usize);
            for chunk in buf[..info.buffer_size()].chunks_exact(3) {
                out.extend_from_slice(&[chunk[0], chunk[1], chunk[2], 255]);
            }
            out
        }
        png::ColorType::GrayscaleAlpha => {
            let mut out = Vec::with_capacity((sw * sh * 4) as usize);
            for chunk in buf[..info.buffer_size()].chunks_exact(2) {
                out.extend_from_slice(&[chunk[0], chunk[0], chunk[0], chunk[1]]);
            }
            out
        }
        png::ColorType::Grayscale => {
            let mut out = Vec::with_capacity((sw * sh * 4) as usize);
            for &g in &buf[..info.buffer_size()] {
                out.extend_from_slice(&[g, g, g, 255]);
            }
            out
        }
        // Palette etc. — we ask png to expand on input, but if not
        // we drop the logo rather than misrender it.
        _ => return None,
    };

    // Nearest-neighbour scale to target×target.
    let mut dst = vec![0u8; (target * target * 4) as usize];
    for dy in 0..target {
        let sy = ((dy as u64) * (sh as u64) / target as u64) as u32;
        for dx in 0..target {
            let sx = ((dx as u64) * (sw as u64) / target as u64) as u32;
            let si = ((sy * sw + sx) * 4) as usize;
            let di = ((dy * target + dx) * 4) as usize;
            if si + 4 <= src_rgba.len() {
                dst[di..di + 4].copy_from_slice(&src_rgba[si..si + 4]);
            }
        }
    }
    Some(DecodedLogo {
        width: target,
        height: target,
        rgba: dst,
    })
}

/// Alpha-composite a straight RGBA8 source onto the OG pixmap at
/// `(x, y)`. Clips to the pixmap bounds. tiny-skia stores pixels
/// pre-multiplied; we mul on the way in.
fn blit_rgba(pixmap: &mut Pixmap, src: &[u8], sw: u32, sh: u32, x: i32, y: i32) {
    let pw = pixmap.width() as i32;
    let ph = pixmap.height() as i32;
    let stride = pixmap.width() as usize;
    let data = pixmap.data_mut();
    for j in 0..(sh as i32) {
        let ty = y + j;
        if ty < 0 || ty >= ph {
            continue;
        }
        for i in 0..(sw as i32) {
            let tx = x + i;
            if tx < 0 || tx >= pw {
                continue;
            }
            let si = ((j as usize) * (sw as usize) + i as usize) * 4;
            if si + 4 > src.len() {
                continue;
            }
            let sr = src[si] as u32;
            let sg = src[si + 1] as u32;
            let sb = src[si + 2] as u32;
            let sa = src[si + 3] as u32;
            if sa == 0 {
                continue;
            }
            // Pre-multiply source.
            let pr = (sr * sa + 127) / 255;
            let pg = (sg * sa + 127) / 255;
            let pb = (sb * sa + 127) / 255;
            let di = (ty as usize * stride + tx as usize) * 4;
            let dr = data[di] as u32;
            let dg = data[di + 1] as u32;
            let db = data[di + 2] as u32;
            let da = data[di + 3] as u32;
            let inv = 255 - sa;
            data[di] = ((pr + (dr * inv + 127) / 255).min(255)) as u8;
            data[di + 1] = ((pg + (dg * inv + 127) / 255).min(255)) as u8;
            data[di + 2] = ((pb + (db * inv + 127) / 255).min(255)) as u8;
            data[di + 3] = ((da + ((255 - da) * sa + 127) / 255).min(255)) as u8;
        }
    }
}

/// Fill a rounded rectangle. Used for the kind pill + footer pill.
/// `radius` is clamped to half the shorter side so callers can pass
/// `999.0` to mean "fully rounded" without thinking about it.
fn fill_rounded_rect(
    pixmap: &mut Pixmap,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    radius: f32,
    color: Color,
) {
    let r = radius.min(w / 2.0).min(h / 2.0).max(0.0);
    let mut paint = Paint::default();
    paint.set_color(color);
    paint.anti_alias = true;
    let mut pb = PathBuilder::new();
    pb.move_to(x + r, y);
    pb.line_to(x + w - r, y);
    pb.quad_to(x + w, y, x + w, y + r);
    pb.line_to(x + w, y + h - r);
    pb.quad_to(x + w, y + h, x + w - r, y + h);
    pb.line_to(x + r, y + h);
    pb.quad_to(x, y + h, x, y + h - r);
    pb.line_to(x, y + r);
    pb.quad_to(x, y, x + r, y);
    pb.close();
    if let Some(path) = pb.finish() {
        pixmap.fill_path(
            &path,
            &paint,
            tiny_skia::FillRule::Winding,
            Transform::identity(),
            None,
        );
    }
}

/// Stroke a rounded rectangle (used for the white footer pill's
/// hairline border).
#[allow(clippy::too_many_arguments)]
fn stroke_rounded_rect(
    pixmap: &mut Pixmap,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    radius: f32,
    color: Color,
    stroke_w: f32,
) {
    let r = radius.min(w / 2.0).min(h / 2.0).max(0.0);
    let mut paint = Paint::default();
    paint.set_color(color);
    paint.anti_alias = true;
    let stroke = Stroke {
        width: stroke_w,
        ..Default::default()
    };
    let mut pb = PathBuilder::new();
    pb.move_to(x + r, y);
    pb.line_to(x + w - r, y);
    pb.quad_to(x + w, y, x + w, y + r);
    pb.line_to(x + w, y + h - r);
    pb.quad_to(x + w, y + h, x + w - r, y + h);
    pb.line_to(x + r, y + h);
    pb.quad_to(x, y + h, x, y + h - r);
    pb.line_to(x, y + r);
    pb.quad_to(x, y, x + r, y);
    pb.close();
    if let Some(path) = pb.finish() {
        pixmap.stroke_path(&path, &paint, &stroke, Transform::identity(), None);
    }
}

/// Filled circle for the orange accent dot inside the footer pill.
fn fill_circle(pixmap: &mut Pixmap, cx: f32, cy: f32, r: f32, color: Color) {
    let mut paint = Paint::default();
    paint.set_color(color);
    paint.anti_alias = true;
    let mut pb = PathBuilder::new();
    pb.push_circle(cx, cy, r);
    if let Some(path) = pb.finish() {
        pixmap.fill_path(
            &path,
            &paint,
            tiny_skia::FillRule::Winding,
            Transform::identity(),
            None,
        );
    }
}

/// Encode the pixmap to PNG bytes via the `png` crate. `tiny-skia`
/// has its own `encode_png` but it's behind the optional `png-format`
/// feature; we go direct to keep the dep tree slim.
fn encode_png(pixmap: &Pixmap) -> Result<Vec<u8>, String> {
    let mut buf: Vec<u8> = Vec::with_capacity((pixmap.width() * pixmap.height() * 4) as usize);
    {
        let mut encoder = png::Encoder::new(Cursor::new(&mut buf), pixmap.width(), pixmap.height());
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder
            .write_header()
            .map_err(|e| format!("png header: {e}"))?;
        // tiny-skia stores RGBA premultiplied. For PNG output we want
        // straight (un-premultiplied) RGBA so partial-alpha edges
        // don't look "burned" in viewers. The card uses only opaque
        // fills + opaque text, so the data is already effectively
        // straight; we still demultiply per-pixel for correctness on
        // anti-aliased text edges.
        let mut straight: Vec<u8> = Vec::with_capacity(pixmap.data().len());
        for chunk in pixmap.data().chunks_exact(4) {
            let r = chunk[0] as u32;
            let g = chunk[1] as u32;
            let b = chunk[2] as u32;
            let a = chunk[3] as u32;
            if a == 0 {
                straight.extend_from_slice(&[0, 0, 0, 0]);
            } else if a == 255 {
                straight.extend_from_slice(&[r as u8, g as u8, b as u8, 255]);
            } else {
                let r = ((r * 255 + a / 2) / a).min(255) as u8;
                let g = ((g * 255 + a / 2) / a).min(255) as u8;
                let b = ((b * 255 + a / 2) / a).min(255) as u8;
                straight.extend_from_slice(&[r, g, b, a as u8]);
            }
        }
        writer
            .write_image_data(&straight)
            .map_err(|e| format!("png data: {e}"))?;
    }
    Ok(buf)
}

fn rgb(c: [u8; 3]) -> Color {
    Color::from_rgba8(c[0], c[1], c[2], 255)
}

/// Measure the rendered width of a single line at the given size.
/// Sums the advance of every glyph through fontdue's metrics —
/// matches what `Layout` produces for a one-line, no-wrap pass.
fn measure_text_width(text: &str, size: f32, font: &Font) -> f32 {
    let mut w = 0.0_f32;
    for ch in text.chars() {
        let metrics = font.metrics(ch, size);
        w += metrics.advance_width;
    }
    w
}

/// Pick the largest font size in `[min, max]` (stepping down by 2 px
/// at a time) that fits `text` within `max_w` × `max_lines`. Lets the
/// title block shrink for long claims without overflowing the card.
#[allow(dead_code)]
fn fit_text_size(
    text: &str,
    font: &Font,
    max_w: f32,
    max_lines: usize,
    max_size: f32,
    min_size: f32,
) -> f32 {
    let mut size = max_size;
    while size > min_size + 0.1 {
        if count_wrapped_lines(text, font, size, max_w) <= max_lines {
            return size;
        }
        size -= 2.0;
    }
    min_size
}

/// Like `fit_text_size` but also enforces a hard vertical-band
/// budget. Title cards need both: at most `max_lines` wrapped lines
/// AND a total block height (`line_count * size * 1.2`) under
/// `band_h`. Steps down by 2 px until both bounds are satisfied or
/// `min_size` is reached.
#[allow(clippy::too_many_arguments)]
fn fit_text_size_in_band(
    text: &str,
    font: &Font,
    max_w: f32,
    max_lines: usize,
    band_h: f32,
    max_size: f32,
    min_size: f32,
) -> f32 {
    let mut size = max_size;
    while size > min_size + 0.1 {
        let lines = count_wrapped_lines(text, font, size, max_w);
        let height = lines as f32 * size * 1.2;
        if lines <= max_lines && height <= band_h {
            return size;
        }
        size -= 2.0;
    }
    min_size
}

/// Use fontdue's layout engine to count how many lines `text` would
/// wrap into at the given size + width. Slightly heavier than
/// approximating with `measure_text_width / max_w * line_count`, but
/// reliable for long unbreakable words.
fn count_wrapped_lines(text: &str, font: &Font, size: f32, max_w: f32) -> usize {
    let mut layout = Layout::new(CoordinateSystem::PositiveYDown);
    layout.reset(&LayoutSettings {
        max_width: Some(max_w),
        wrap_style: WrapStyle::Word,
        ..LayoutSettings::default()
    });
    layout.append(&[font], &TextStyle::new(text, size, 0));
    layout.lines().map(|l| l.len()).unwrap_or(1)
}

/// Render `text` onto the pixmap with simple word-wrap + ellipsis
/// truncation. `(x, y)` is the top-left of the first glyph baseline
/// box. `max_w` is the horizontal bound; `max_lines` caps vertical
/// growth — any overflow line gets replaced by an ellipsis.
#[allow(clippy::too_many_arguments)]
fn draw_text(
    pixmap: &mut Pixmap,
    text: &str,
    x: f32,
    y: f32,
    size: f32,
    font: &Font,
    color: Color,
    max_w: f32,
    max_lines: usize,
) {
    // First pass — let fontdue lay out the entire string so we know
    // where every glyph lands. We then walk the line list, only
    // drawing up to `max_lines` of them.
    let mut layout = Layout::new(CoordinateSystem::PositiveYDown);
    layout.reset(&LayoutSettings {
        x,
        y,
        max_width: Some(max_w),
        line_height: 1.2,
        wrap_style: WrapStyle::Word,
        ..LayoutSettings::default()
    });
    layout.append(&[font], &TextStyle::new(text, size, 0));
    let glyphs: Vec<_> = layout.glyphs().clone();
    let lines = layout.lines().cloned().unwrap_or_default();

    let kept_lines = lines.len().min(max_lines);
    let needs_ellipsis = lines.len() > max_lines;

    for (line_idx, line) in lines.iter().enumerate().take(kept_lines) {
        let line_glyphs = &glyphs[line.glyph_start..=line.glyph_end];
        // If this is the last kept line AND there are wrapped lines
        // we're dropping, append an ellipsis. We do that by
        // re-rendering the line: drop trailing glyphs until "...""
        // fits, then draw the truncated line + ellipsis.
        if line_idx + 1 == kept_lines && needs_ellipsis {
            draw_line_with_ellipsis(pixmap, line_glyphs, font, size, color, max_w);
        } else {
            for gp in line_glyphs {
                blit_glyph(pixmap, font, gp.parent, gp.x, gp.y, size, color);
            }
        }
    }
}

fn draw_line_with_ellipsis(
    pixmap: &mut Pixmap,
    glyphs: &[fontdue::layout::GlyphPosition],
    font: &Font,
    size: f32,
    color: Color,
    max_w: f32,
) {
    let ellipsis = '…';
    let ell_metrics = font.metrics(ellipsis, size);
    // Drop glyphs from the tail until "..." fits on the line.
    let mut end = glyphs.len();
    while end > 0 {
        let last = &glyphs[end - 1];
        let line_w = last.x + last.width as f32 - glyphs[0].x;
        if line_w + ell_metrics.advance_width <= max_w {
            break;
        }
        end -= 1;
    }
    for gp in &glyphs[..end] {
        blit_glyph(pixmap, font, gp.parent, gp.x, gp.y, size, color);
    }
    let (ex, ey) = glyphs
        .get(end.saturating_sub(1))
        .map(|g| (g.x + g.width as f32, g.y))
        .unwrap_or((glyphs.first().map(|g| g.x).unwrap_or(0.0), 0.0));
    blit_glyph(pixmap, font, ellipsis, ex, ey, size, color);
}

/// Rasterise a single glyph through fontdue and alpha-composite it
/// onto the pixmap at `(x, y)`. fontdue returns an 8-bit coverage
/// bitmap; we multiply that with the requested colour's alpha and
/// blend over whatever is underneath.
fn blit_glyph(pixmap: &mut Pixmap, font: &Font, ch: char, x: f32, y: f32, size: f32, color: Color) {
    let (metrics, bitmap) = font.rasterize(ch, size);
    if metrics.width == 0 || metrics.height == 0 {
        return;
    }
    let px = x.round() as i32;
    let py = y.round() as i32;
    let w = pixmap.width() as i32;
    let h = pixmap.height() as i32;
    let stride = pixmap.width() as usize;
    let data = pixmap.data_mut();
    let cr = (color.red() * 255.0) as u32;
    let cg = (color.green() * 255.0) as u32;
    let cb = (color.blue() * 255.0) as u32;
    for j in 0..metrics.height {
        let target_y = py + j as i32;
        if target_y < 0 || target_y >= h {
            continue;
        }
        for i in 0..metrics.width {
            let target_x = px + i as i32;
            if target_x < 0 || target_x >= w {
                continue;
            }
            let cov = bitmap[j * metrics.width + i] as u32;
            if cov == 0 {
                continue;
            }
            let idx = (target_y as usize * stride + target_x as usize) * 4;
            let dr = data[idx] as u32;
            let dg = data[idx + 1] as u32;
            let db = data[idx + 2] as u32;
            let da = data[idx + 3] as u32;
            // src.a = coverage; src is opaque colour pre-multiplied
            // by coverage. Standard "src-over" composite.
            let sa = cov;
            let inv = 255 - sa;
            data[idx] = (((cr * sa) + dr * inv + 127) / 255).min(255) as u8;
            data[idx + 1] = (((cg * sa) + dg * inv + 127) / 255).min(255) as u8;
            data[idx + 2] = (((cb * sa) + db * inv + 127) / 255).min(255) as u8;
            data[idx + 3] = (da + ((255 - da) * sa + 127) / 255).min(255) as u8;
        }
    }
}

// --- GCS cache layer -------------------------------------------------------

/// Cache-key version. Bump when the OG *template or CSS* changes (not the
/// per-entity content, which `content_hash` already covers) so every cached
/// card is invalidated at once. Without this, a reskin can't bust the cache
/// — the content hash is unchanged, so stale pre-reskin PNGs keep serving
/// until each entity is edited. Bumped to 2 for the 8-bit arcade reskin
/// (the pre-reskin cream cards had no version and never expired).
pub const OG_CACHE_VERSION: u32 = 2;

/// GCS object key for the cached card. `kind` is `"claims"` or
/// `"battles"`; the `id` portion is the entity UUID (stable), `content_hash`
/// busts the cache on every edit, and `OG_CACHE_VERSION` busts it on every
/// template/CSS change.
pub fn cache_object_name(kind: &str, id: &str, content_hash: &str) -> String {
    format!("og/{kind}/{id}-{content_hash}-v{OG_CACHE_VERSION}.png")
}

/// Attempt to fetch the cached PNG from the configured blob store.
/// Returns `Some(bytes)` on hit, `None` on any miss (404, network
/// error, dev-mode store with no real backend). Soft failure — the
/// route layer falls through to generation + upload on `None`.
pub async fn fetch_cached(blob_store_url: &str, object_name: &str) -> Option<Vec<u8>> {
    let bucket = blob_store_url.strip_prefix("gs://")?;
    let url = format!(
        "https://storage.googleapis.com/{}/{}",
        bucket,
        urlencoding::encode(object_name)
    );
    let client = reqwest::Client::new();
    let mut req = client.get(&url);
    if let Ok(token) = gcp_access_token().await {
        req = req.header("authorization", format!("Bearer {}", token));
    }
    let resp = req.send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    resp.bytes().await.ok().map(|b| b.to_vec())
}

/// Upload generated PNG bytes to GCS under the given object name.
/// Returns `Ok(())` on success, an error string otherwise. Best-effort
/// from the caller's POV — we still serve the freshly-generated bytes
/// even if the cache write fails.
pub async fn upload_to_cache(
    blob_store_url: &str,
    object_name: &str,
    data: &[u8],
) -> Result<(), String> {
    let Some(bucket) = blob_store_url.strip_prefix("gs://") else {
        // Dev fallback — no actual write. Keep the trace so we can
        // see why local cards always regenerate.
        tracing::debug!(
            object = %object_name,
            "og_image upload: dev mode, skipped"
        );
        return Ok(());
    };
    let token = gcp_access_token()
        .await
        .map_err(|e| format!("gcp token: {e}"))?;
    let url = format!(
        "https://storage.googleapis.com/upload/storage/v1/b/{}/o?uploadType=media&name={}",
        bucket,
        urlencoding::encode(object_name)
    );
    let client = reqwest::Client::new();
    let resp = client
        .post(&url)
        .header("authorization", format!("Bearer {}", token))
        .header("content-type", "image/png")
        // Matches the served header (routes/og.rs png_response): the
        // stable og:image URL's content changes, so not immutable.
        .header(
            "cache-control",
            "public, max-age=300, stale-while-revalidate=86400",
        )
        .body(data.to_vec())
        .send()
        .await
        .map_err(|e| format!("gcs upload: {e}"))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("gcs upload {status}: {body}"));
    }
    Ok(())
}

// --- Headless-Chromium render service client (since 0.22.0) --------------

/// Render an OG card by screenshotting the `og-html` template via the
/// `og-render` microservice (headless Chromium). `render_base` is the
/// service's base URL (the `OG_RENDER_URL` env value, e.g.
/// `http://og-render:8080`); `og_html_path` is the API-relative
/// template path the service should navigate to and screenshot, e.g.
/// `/og-html/battles/<uuid>`.
///
/// The render service resolves the og-html URL against its OWN
/// `KRAB_HTML_BASE` env (pointing back at this API), so we only pass
/// the path. Returns the PNG bytes on a 200, or `Err` on any failure
/// (timeout, non-200, malformed body) — the caller MUST fall back to
/// the `tiny_skia` `render_png` on `Err` so a slow/down render service
/// never breaks social previews.
///
/// A tight client-side timeout (5s) bounds the worst case: a scraper
/// hitting `/og/...` waits at most ~5s before we abandon Chromium and
/// paint the local card.
pub async fn render_via_service(render_base: &str, og_html_path: &str) -> Result<Vec<u8>, String> {
    let base = render_base.trim_end_matches('/');
    let url = format!("{base}/render?path={}", urlencoding::encode(og_html_path));
    // Headless-Chromium renders block on every leaderboard logo `<img>`
    // loading. A battle card carries up to 5 rows of vendor logos, so a
    // 5s budget routinely timed out on the heavier battle cards — the
    // render call erred and the handler silently fell back to the
    // leaderboard-less tiny_skia card. 20s gives the logo-heavy battle
    // card room while staying well under Cloud Run's request timeout.
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .map_err(|e| format!("build render client: {e}"))?;
    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("render request: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("render service status {}", resp.status()));
    }
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| format!("render body: {e}"))?
        .to_vec();
    if !bytes.starts_with(&[0x89, 0x50, 0x4e, 0x47]) {
        return Err("render service returned non-PNG body".to_string());
    }
    Ok(bytes)
}

/// Per-domain logo cache key. Stable sha256 over the source URL so
/// `https://www.google.com/s2/favicons?domain=foo.com&sz=64` and any
/// variant URL each map to a unique object — but the same vendor
/// across battles reuses the bytes once they're warm in GCS.
fn logo_cache_object(url: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(url.as_bytes());
    let digest = h.finalize();
    format!("logos/{}.png", hex::encode(&digest[..16]))
}

/// Fetch one logo's PNG bytes, trying the per-URL GCS cache first
/// (`gs://<blob_store>/logos/<sha>.png`) and falling back to a direct
/// network fetch of the source URL. On a network success the bytes
/// are best-effort uploaded to the GCS cache so the next render
/// reuses them. Returns `None` for any failure — the OG renderer
/// treats a missing logo as "skip this slot in the strip", never
/// 5xx.
pub async fn fetch_logo_bytes(blob_store_url: &str, source_url: &str) -> Option<Vec<u8>> {
    let object_name = logo_cache_object(source_url);
    if let Some(bytes) = fetch_cached(blob_store_url, &object_name).await
        && bytes.starts_with(&[0x89, 0x50, 0x4e, 0x47])
    {
        return Some(bytes);
    }
    // Cache miss → fetch from origin.
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(8))
        .user_agent("KrabArena-OG-Logo/0.1")
        .build()
        .ok()?;
    let resp = client.get(source_url).send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let bytes = resp.bytes().await.ok()?.to_vec();
    // Only cache + return real PNG bytes. Google S2 occasionally
    // returns a default fallback icon as a small GIF; we drop those
    // on the floor so the OG renderer's PNG decoder doesn't fail at
    // composite time.
    if !bytes.starts_with(&[0x89, 0x50, 0x4e, 0x47]) {
        return None;
    }
    // Best-effort cache write; we don't block the OG response on it.
    let store = blob_store_url.to_string();
    let obj = object_name.clone();
    let upload_bytes = bytes.clone();
    tokio::spawn(async move {
        if let Err(e) = upload_to_cache(&store, &obj, &upload_bytes).await {
            tracing::debug!(object = %obj, error = %e, "og_image: logo cache write failed");
        }
    });
    Some(bytes)
}

/// Metadata-server access token. Same flow `artifact_service` uses,
/// kept private here so the OG image module is self-contained.
async fn gcp_access_token() -> Result<String, anyhow::Error> {
    let client = reqwest::Client::new();
    let resp = client
        .get("http://metadata.google.internal/computeMetadata/v1/instance/service-accounts/default/token")
        .header("Metadata-Flavor", "Google")
        .send()
        .await?;
    let body: serde_json::Value = resp.json().await?;
    body["access_token"]
        .as_str()
        .map(|s| s.to_string())
        .ok_or_else(|| anyhow::anyhow!("no access_token in metadata response"))
}

/// Whether the bundled JetBrains Mono font parses. Currently exposed
/// only so the test suite confirms the font ships; not called from
/// the render path yet. Kept here so the `font_mono` lazy slot has at
/// least one keeper — future revisions may render footer URLs or
/// version tags in JBM.
#[cfg(test)]
fn mono_font_loads() -> bool {
    let _ = font_mono();
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_png_returns_png_bytes_for_minimal_card() {
        let card = OgCard {
            title: "What is the best Web Access API?".into(),
            subtitle: "Sponsored by Keenable. 12 claims, 8 verified.".into(),
            footer: Some("Winner: Exa Search".into()),
            kind_label: "BATTLE".into(),
            logos: vec![],
            hero_value: None,
            hero_label: None,
            refuted: false,
            refutation_line: None,
        };
        let bytes = render_png(&card).expect("render_png succeeds");
        // PNG magic bytes — first 8 bytes are 89 50 4E 47 0D 0A 1A 0A.
        assert_eq!(
            &bytes[..8],
            &[0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a],
            "encoded bytes start with PNG magic"
        );
        // Sanity: at least a few KB — a 1200x630 RGBA image with text
        // shouldn't compress below 5 KB.
        assert!(
            bytes.len() > 5_000,
            "PNG body looks too small: {} bytes",
            bytes.len()
        );
    }

    #[test]
    fn render_png_handles_very_long_title_without_overflow() {
        let card = OgCard {
            title: "A".repeat(200),
            subtitle: "B".repeat(400),
            footer: Some("C".repeat(120)),
            kind_label: "CLAIM".into(),
            logos: vec![],
            hero_value: None,
            hero_label: None,
            refuted: false,
            refutation_line: None,
        };
        let bytes = render_png(&card).expect("render long card");
        assert_eq!(
            &bytes[..8],
            &[0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]
        );
    }

    #[test]
    fn content_hash_changes_when_title_changes() {
        let a = OgCard {
            title: "Title A".into(),
            subtitle: "sub".into(),
            footer: None,
            kind_label: "BATTLE".into(),
            logos: vec![],
            hero_value: None,
            hero_label: None,
            refuted: false,
            refutation_line: None,
        };
        let b = OgCard {
            title: "Title B".into(),
            subtitle: "sub".into(),
            footer: None,
            kind_label: "BATTLE".into(),
            logos: vec![],
            hero_value: None,
            hero_label: None,
            refuted: false,
            refutation_line: None,
        };
        assert_ne!(a.content_hash(), b.content_hash());
    }

    #[test]
    fn content_hash_is_stable_for_identical_input() {
        let a = OgCard {
            title: "same".into(),
            subtitle: "same".into(),
            footer: Some("same".into()),
            kind_label: "CLAIM".into(),
            logos: vec![],
            hero_value: Some("1.09x".into()),
            hero_label: Some("measured".into()),
            refuted: true,
            refutation_line: None,
        };
        assert_eq!(a.content_hash(), a.clone().content_hash());
    }

    #[test]
    fn content_hash_changes_when_hero_value_differs() {
        // Two cards differing ONLY in `hero_value` must hash
        // differently — this is what busts the GCS cache when the
        // reviewer regenerates a claim's presentation with a new
        // headline number.
        let base = OgCard {
            title: "same".into(),
            subtitle: "same".into(),
            footer: None,
            kind_label: "CLAIM".into(),
            logos: vec![],
            hero_value: Some("1.09x".into()),
            hero_label: Some("measured".into()),
            refuted: false,
            refutation_line: None,
        };
        let mut changed = base.clone();
        changed.hero_value = Some("2.5x".into());
        assert_ne!(
            base.content_hash(),
            changed.content_hash(),
            "hero_value change must bust the cache"
        );
    }

    #[test]
    fn content_hash_changes_when_refuted_flips() {
        let base = OgCard {
            title: "same".into(),
            subtitle: "same".into(),
            footer: None,
            kind_label: "CLAIM".into(),
            logos: vec![],
            hero_value: Some("1.09x".into()),
            hero_label: Some("measured".into()),
            refuted: false,
            refutation_line: None,
        };
        let mut refuted = base.clone();
        refuted.refuted = true;
        assert_ne!(
            base.content_hash(),
            refuted.content_hash(),
            "flipping refuted must bust the cache"
        );
    }

    #[test]
    fn content_hash_changes_when_refutation_line_differs() {
        // A presentation carrying BOTH hero_stat and refutation renders
        // hero_value/hero_label from the hero_stat — the refutation's
        // claimed/measured strings are otherwise invisible to the hash.
        // The Chromium `/og-html/claims/{id}` card renders those strings
        // directly, so a change to just the refutation text must still
        // bust the cache key or the render service serves a stale PNG.
        let base = OgCard {
            title: "same".into(),
            subtitle: "same".into(),
            footer: None,
            kind_label: "CLAIM".into(),
            logos: vec![],
            hero_value: Some("1.09x".into()),
            hero_label: Some("measured".into()),
            refuted: true,
            refutation_line: Some("6.4x vs 1.09x".into()),
        };
        let mut changed = base.clone();
        changed.refutation_line = Some("6.4x vs 2.2x".into());
        assert_ne!(
            base.content_hash(),
            changed.content_hash(),
            "refutation_line change must bust the cache even when hero fields are unchanged"
        );
    }

    #[test]
    fn cache_object_name_layout_matches_spec() {
        assert_eq!(
            cache_object_name("claims", "abc-123", "deadbeef"),
            format!("og/claims/abc-123-deadbeef-v{OG_CACHE_VERSION}.png")
        );
        assert_eq!(
            cache_object_name("battles", "xyz", "cafebabe"),
            format!("og/battles/xyz-cafebabe-v{OG_CACHE_VERSION}.png")
        );
    }

    #[test]
    fn bundled_inter_and_jetbrains_mono_parse() {
        // Tripwires for the "fonts shipped" invariant — if any font
        // file goes missing from the bundle, this test breaks at
        // build time (include_bytes!) or here (Font::from_bytes).
        let _ = font_black().glyph_count();
        let _ = font_bold().glyph_count();
        let _ = font_medium().glyph_count();
        let _ = font_regular().glyph_count();
        assert!(mono_font_loads());
    }

    #[test]
    fn render_png_draws_refuted_hero_card_without_panicking() {
        let card = OgCard {
            title: "Nobody said it was 6.4x faster".into(),
            subtitle: "KrabReviewer re-ran the benchmark and the number didn't hold up.".into(),
            footer: Some("0 verified · 1 refuted".into()),
            kind_label: "HYPOTHESIS REFUTED".into(),
            logos: vec![],
            hero_value: Some("1.09×".into()),
            hero_label: Some("measured vs 6.4× claimed".into()),
            refuted: true,
            refutation_line: None,
        };
        let bytes = render_png(&card).expect("refuted hero card renders");
        assert_eq!(
            &bytes[..8],
            &[0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]
        );
    }

    #[test]
    fn bundled_crab_logo_decodes() {
        // Tripwire for the "crab logo asset shipped" invariant —
        // ensures the OG renderer's header band always has something
        // to draw and never silently swallows a decode failure.
        let bmp = crab_logo_bitmap(80).expect("crab logo decodes at 80 px");
        assert_eq!(bmp.width, 80);
        assert_eq!(bmp.height, 80);
    }
}

#[cfg(test)]
mod preview_tests {
    use super::*;
    use std::io::Write;

    /// Manual preview helper — run with
    ///   cargo test -p krabarena-api --lib services::og_image::preview_tests -- --ignored --nocapture
    /// then open /tmp/og-sample-*.png to inspect the cards.
    #[test]
    #[ignore]
    fn write_sample_cards_to_tmp() {
        let battle = OgCard {
            title: "What is the best Web Access API for AI Agents?".into(),
            subtitle: "Sponsored by Keenable. Exa, Brave, Perplexity, You.com tested across 50 factual queries with p50/p95 latency.".into(),
            footer: Some("ai · search · benchmarks".into()),
            kind_label: "Battle".into(),
            logos: vec![],
            hero_value: None,
            hero_label: None,
            refuted: false,
            refutation_line: None,
        };
        let bytes = render_png(&battle).unwrap();
        let mut f = std::fs::File::create("/tmp/og-sample-battle.png").unwrap();
        f.write_all(&bytes).unwrap();
        println!("wrote /tmp/og-sample-battle.png ({} bytes)", bytes.len());

        let claim = OgCard {
            title: "Exa Search wins on factual recall with sub-300ms p50 latency".into(),
            subtitle: "Tested 50 factual queries against 4 web access APIs. Exa won 38/50 with mean latency 287ms vs Brave 412ms and Perplexity 612ms.".into(),
            footer: Some("Winner: Exa Search".into()),
            kind_label: "Claim".into(),
            logos: vec![],
            hero_value: None,
            hero_label: None,
            refuted: false,
            refutation_line: None,
        };
        let bytes = render_png(&claim).unwrap();
        let mut f = std::fs::File::create("/tmp/og-sample-claim.png").unwrap();
        f.write_all(&bytes).unwrap();
        println!("wrote /tmp/og-sample-claim.png ({} bytes)", bytes.len());

        let refuted = OgCard {
            title: "Nobody said it was 6.4x faster".into(),
            subtitle: "KrabReviewer re-ran the benchmark and the number didn't hold up.".into(),
            footer: Some("0 verified · 1 refuted".into()),
            kind_label: "HYPOTHESIS REFUTED".into(),
            logos: vec![],
            hero_value: Some("1.09×".into()),
            hero_label: Some("measured vs 6.4× claimed".into()),
            refuted: true,
            refutation_line: None,
        };
        let bytes = render_png(&refuted).unwrap();
        let mut f = std::fs::File::create("/tmp/og-sample-refuted.png").unwrap();
        f.write_all(&bytes).unwrap();
        println!("wrote /tmp/og-sample-refuted.png ({} bytes)", bytes.len());
    }
}

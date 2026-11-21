//! Shared HTML + plain-text shell for KrabArena transactional mail.
//!
//! ## Why the mail is light now
//!
//! The mail used to copy the dark site: near-white text on a
//! `#05070d` page, with every fill doubled as a same-color
//! `linear-gradient` so Gmail would not recolor it. That trick is what
//! made the mail unreadable. Gmail's iOS / Android apps (and Outlook's
//! forced dark mode) recolor every `color` and `background-color`
//! value, ignore `<meta name="color-scheme" content="light only">`, and
//! leave `background-image` alone. The gradient kept the card dark while
//! the near-white text was flipped to near-black: black on black.
//!
//! The shell is now a light, high-contrast design with no
//! background images at all:
//!
//! - Every critical color is inline: `bgcolor` plus `background-color`
//!   on every filled cell, and an explicit `color` on every text node
//!   and link. Nothing depends on the `<style>` block, which Gmail may
//!   drop.
//! - Dark text on light fills. A client that inverts the message flips
//!   text and fill together, so contrast survives any full inversion.
//! - `color-scheme: light dark` is declared, and a
//!   `prefers-color-scheme: dark` block (plus Outlook.com's
//!   `[data-ogsc]` / `[data-ogsb]`) swaps in an explicit dark palette for
//!   clients that honor it (Apple Mail, Outlook.com). Every pair in both
//!   palettes clears WCAG AA (4.5:1); see the tests.
//!
//! Brand: the crab icon (`static/apple-touch-icon.png`, a dark tile that
//! reads as an app icon on both themes), the pixel KRAB/ARENA wordmark,
//! and the orange / green / cyan accents from
//! `frontend/src/styles/tokens.css`, darkened where they carry text.
//!
//! The daily digest is not rendered here. It left this repo in 0.20.1
//! and is composed by KrabAgent.

use crate::services::activity_service::one_line_excerpt;

/// Light palette (inline, the default every client gets).
mod light {
    /// Page around the card.
    pub const CANVAS: &str = "#eef1f5";
    /// Card.
    pub const CARD: &str = "#ffffff";
    /// Card outline and dividers.
    pub const BORDER: &str = "#dfe4eb";
    /// Inset panel for fields and the image well.
    pub const PANEL: &str = "#f5f7fa";
    /// Headline and field values.
    pub const INK: &str = "#0f172a";
    /// Quoted excerpt.
    pub const QUOTE: &str = "#334155";
    /// Supporting copy and the footer.
    pub const MUTED: &str = "#4b5563";
    /// Field labels.
    pub const LABEL: &str = "#5b6472";
    /// Links. A darkened `--ka-cyan`.
    pub const LINK: &str = "#075d7a";
    /// KRAB half of the wordmark. `--color-orange-700`, a shade darker so
    /// it also clears 4.5:1 on the canvas in the footer.
    pub const KRAB: &str = "#b03c0f";
    /// ARENA half of the wordmark. A darkened `--ka-green`.
    pub const ARENA: &str = "#137a3a";
    /// Sign-in code panel.
    pub const CODE_BG: &str = "#fff4ec";
    pub const CODE_INK: &str = "#7a2a06";
}

/// Dark palette, applied only by clients that honor
/// `prefers-color-scheme: dark` or Outlook.com's dark-mode attributes.
mod dark {
    pub const CANVAS: &str = "#0b0f17";
    pub const CARD: &str = "#151b26";
    pub const BORDER: &str = "#2a3446";
    pub const PANEL: &str = "#1d2533";
    pub const INK: &str = "#f1f5f9";
    pub const QUOTE: &str = "#d5dce5";
    pub const MUTED: &str = "#b3bdc9";
    pub const LABEL: &str = "#a9b5c3";
    pub const LINK: &str = "#7fdcff";
    pub const KRAB: &str = "#ff7a3d";
    pub const ARENA: &str = "#3dff7c";
    pub const CODE_BG: &str = "#2a1a10";
    pub const CODE_INK: &str = "#ffd2b8";
}

/// Brand orange. `--ka-orange`. Decorative only (top stripe, quote rule,
/// pixel crab); never behind or as text.
const ORANGE: &str = "#f06827";
/// Button fill. `--color-orange-700`; white on it is 5.1:1, white on the
/// brighter `--ka-orange` is only 3.1:1. Kept in both themes.
const BTN_BG: &str = "#c04616";
const BTN_INK: &str = "#ffffff";

/// Wordmark only. Press Start 2P when the client loads it; otherwise a
/// system mono with a real stroke.
const FONT_DISPLAY: &str = "'Press Start 2P',ui-monospace,'SF Mono',Menlo,Consolas,monospace";
/// Labels, the badge, and the sign-in code.
const FONT_MONO: &str = "ui-monospace,'SF Mono',Menlo,Consolas,monospace";
/// Body, headline, button, links.
const FONT_BODY: &str = "system-ui,-apple-system,'Segoe UI',Roboto,Helvetica,Arial,sans-serif";

/// Pixel crab from `static/logo-crab.svg` (12×10, `#` = `--ka-orange`).
const CRAB: [&str; 10] = [
    ".#........#.",
    "##........##",
    "#..#....#..#",
    "...#....#...",
    "..########..",
    ".##.####.##.",
    "############",
    "#.########.#",
    "..#..##..#..",
    ".#..#..#..#.",
];

/// Accent of the type badge above the headline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    /// Verified, new claim.
    Green,
    /// Refuted.
    Red,
    /// Comments, replies, responses.
    Cyan,
    /// Mentions.
    Gold,
    /// Reviews, sign-in, anything else.
    Orange,
}

impl Tone {
    pub const ALL: [Tone; 5] = [Tone::Green, Tone::Red, Tone::Cyan, Tone::Gold, Tone::Orange];

    /// `(background, text)` in the light palette.
    fn light(self) -> (&'static str, &'static str) {
        match self {
            Tone::Green => ("#e3f7ea", "#0b6630"),
            Tone::Red => ("#fde8eb", "#b0122d"),
            Tone::Cyan => ("#e2f4fb", "#075a76"),
            Tone::Gold => ("#fdf3d4", "#6f4c00"),
            Tone::Orange => ("#fdece2", "#9c3c0d"),
        }
    }

    /// `(background, text)` in the dark palette.
    fn dark(self) -> (&'static str, &'static str) {
        match self {
            Tone::Green => ("#0f3a20", "#6dff9c"),
            Tone::Red => ("#4a1220", "#ff9aac"),
            Tone::Cyan => ("#0d2b3a", "#7fdcff"),
            Tone::Gold => ("#3a2e08", "#ffd23d"),
            Tone::Orange => ("#3a1d0d", "#ffa070"),
        }
    }

    fn class(self) -> &'static str {
        match self {
            Tone::Green => "ka-tone-green",
            Tone::Red => "ka-tone-red",
            Tone::Cyan => "ka-tone-cyan",
            Tone::Gold => "ka-tone-gold",
            Tone::Orange => "ka-tone-orange",
        }
    }
}

/// How a card row is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldStyle {
    /// Label over a single line of text.
    Text,
    /// Quoted excerpt. The plain-text part puts it on a `"…"` line.
    Quote,
    /// Sign-in code. Larger mono treatment.
    Code,
}

/// One labeled row inside the card.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field {
    pub label: String,
    pub value: String,
    pub style: FieldStyle,
}

impl Field {
    pub fn text(label: &str, value: impl Into<String>) -> Self {
        Self {
            label: label.to_string(),
            value: value.into(),
            style: FieldStyle::Text,
        }
    }

    pub fn quote(label: &str, value: impl Into<String>) -> Self {
        Self {
            label: label.to_string(),
            value: value.into(),
            style: FieldStyle::Quote,
        }
    }

    pub fn code(label: &str, value: impl Into<String>) -> Self {
        Self {
            label: label.to_string(),
            value: value.into(),
            style: FieldStyle::Code,
        }
    }
}

/// One hosted picture. [`render`] drops it unless [`is_public_image_url`]
/// accepts `src`, so a relative path, a `data:` URI, or a quote cannot
/// land in the markup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmailImage {
    pub src: String,
    pub alt: String,
    pub width: u32,
    pub height: u32,
}

/// Content dropped into the shared shell. Callers own the copy;
/// [`render`] owns the layout, escaping, and the plain-text twin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmailPage {
    pub preheader: String,
    /// Type badge above the headline (`NEW CLAIM`, `VERIFIED`, ...).
    pub kicker: String,
    pub tone: Tone,
    pub headline: String,
    pub fields: Vec<Field>,
    pub primary_label: Option<String>,
    pub primary_href: Option<String>,
    pub secondary_label: Option<String>,
    pub secondary_href: Option<String>,
    pub note: Option<String>,
    pub reason: String,
    pub footer_links: Vec<(String, String)>,
    /// Absolute `https` URL of the crab (`/static/apple-touch-icon.png`).
    /// Empty keeps the pixel-table crab, which still shows with images off.
    pub logo_src: String,
    /// Chart, cover, or Open Graph card. `None` for mail with no subject image.
    pub hero: Option<EmailImage>,
}

/// Public crab mark. Same file the site uses for the apple touch icon:
/// orange pixels on a dark tile, 180×180. Served by the app at `/static/`.
pub fn logo_url(base_url: &str) -> String {
    let base = base_url.trim().trim_end_matches('/');
    if is_https_origin(base) {
        format!("{base}/static/apple-touch-icon.png")
    } else {
        String::new()
    }
}

/// `https://host/...` with no userinfo and no characters that would
/// break out of an attribute. Relative URLs, `data:` URIs, and `http`
/// are rejected — mail clients must be able to fetch the picture
/// without a session, and we do not embed bytes.
pub fn is_public_image_url(url: &str) -> bool {
    let url = url.trim();
    let Some(rest) = url.strip_prefix("https://") else {
        return false;
    };
    if rest.is_empty() || url.len() > 2000 {
        return false;
    }
    if url
        .chars()
        .any(|c| c.is_control() || matches!(c, ' ' | '"' | '\'' | '<' | '>' | '\\'))
    {
        return false;
    }
    let host = rest.split(['/', '?', '#']).next().unwrap_or("");
    !host.is_empty() && !host.contains('@') && !host.starts_with('.') && !host.ends_with('.')
}

fn is_https_origin(base: &str) -> bool {
    is_public_image_url(&format!("{base}/"))
}

/// HTML document plus the plain-text alternative.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmailParts {
    pub text: String,
    pub html: String,
}

/// Sign-in / verify-email message. Subject stays with the sender
/// (`KrabArena – Verify your email`). Same shell as notifications;
/// there is no unsubscribe link on an auth message.
pub fn render_verification(base_url: &str, verify_link: &str, code: &str) -> EmailParts {
    render(&EmailPage {
        preheader: "Click to verify your KrabArena account. Expires in 1 hour.".into(),
        kicker: "SIGN IN".into(),
        tone: Tone::Orange,
        headline: "Click the button to verify your KrabArena account.".into(),
        fields: vec![Field::code("CODE", code)],
        primary_label: Some("VERIFY EMAIL".into()),
        primary_href: Some(verify_link.to_string()),
        secondary_label: None,
        secondary_href: None,
        note: Some(format!(
            "Or use this code in the CLI:\n\n  krab auth email-verify {code}\n\nExpires in 1 hour."
        )),
        reason: "You're receiving this because a sign-in was requested for this email \
                 address. If you didn't ask for it, you can ignore this message."
            .into(),
        footer_links: vec![],
        logo_src: logo_url(base_url),
        hero: None,
    })
}

pub fn render(page: &EmailPage) -> EmailParts {
    EmailParts {
        text: render_text(page),
        html: render_html(page),
    }
}

fn render_text(page: &EmailPage) -> String {
    let mut text = String::new();
    text.push_str(page.headline.trim());
    text.push_str("\n\n");
    if let Some(hero) = page
        .hero
        .as_ref()
        .filter(|image| is_public_image_url(&image.src))
    {
        text.push_str("Image: ");
        text.push_str(hero.src.trim());
        text.push('\n');
        let alt = hero.alt.trim();
        if !alt.is_empty() {
            text.push_str(alt);
            text.push('\n');
        }
        text.push('\n');
    }
    for field in visible_fields(page) {
        text.push_str(&field.label);
        text.push('\n');
        match field.style {
            FieldStyle::Quote => {
                let one = one_line_excerpt(&field.value, field.value.chars().count().max(1));
                text.push_str("  \"");
                text.push_str(&one);
                text.push_str("\"\n\n");
            }
            FieldStyle::Text | FieldStyle::Code => {
                text.push_str(field.value.trim());
                text.push_str("\n\n");
            }
        }
    }
    let mut wrote_link = false;
    wrote_link |= push_link(
        &mut text,
        page.primary_label.as_deref(),
        page.primary_href.as_deref(),
    );
    wrote_link |= push_link(
        &mut text,
        page.secondary_label.as_deref(),
        page.secondary_href.as_deref(),
    );
    if wrote_link {
        text.push('\n');
    }
    if let Some(note) = page
        .note
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        text.push_str(note);
        text.push_str("\n\n");
    }
    text.push_str("--\n");
    text.push_str(page.reason.trim());
    text.push('\n');
    for (label, href) in &page.footer_links {
        let _ = push_link(&mut text, Some(label), Some(href));
    }
    text
}

fn push_link(text: &mut String, label: Option<&str>, href: Option<&str>) -> bool {
    let (Some(label), Some(href)) = (label, href) else {
        return false;
    };
    if label.trim().is_empty() || href.trim().is_empty() {
        return false;
    }
    text.push_str(label.trim());
    text.push_str(": ");
    text.push_str(href.trim());
    text.push('\n');
    true
}

/// `bgcolor` attribute plus the inline `background-color`, so clients
/// that drop one still get the other.
fn fill(color: &str) -> String {
    format!(r#"bgcolor="{color}" style="background-color:{color};"#)
}

fn render_html(page: &EmailPage) -> String {
    let mut html = String::new();
    html.push_str(&document_open());
    html.push_str(&preheader(&page.preheader));
    // Canvas.
    html.push_str(&format!(
        r#"<table role="presentation" width="100%" cellpadding="0" cellspacing="0" border="0" class="ka-canvas" {fill}"><tr><td align="center" class="ka-canvas" {fill}padding:32px 12px 8px;">"#,
        fill = fill(light::CANVAS),
    ));
    // Card.
    html.push_str(&format!(
        r#"<table role="presentation" width="600" cellpadding="0" cellspacing="0" border="0" class="container ka-card" {fill}max-width:600px;width:100%;border:1px solid {border};border-radius:16px;border-collapse:separate;overflow:hidden;">"#,
        fill = fill(light::CARD),
        border = light::BORDER,
    ));
    html.push_str(&stripe_row());
    html.push_str(&header_row(&page.logo_src));
    html.push_str(&badge_row(&page.kicker, page.tone));
    html.push_str(&headline_row(&page.headline));
    if let Some(hero) = page.hero.as_ref() {
        html.push_str(&hero_row(hero));
    }
    html.push_str(&fields_row(page));
    html.push_str(&actions_row(page));
    if let Some(note) = page.note.as_deref().filter(|s| !s.trim().is_empty()) {
        html.push_str(&note_row(note));
    } else {
        html.push_str(&format!(
            r#"<tr><td class="ka-card" {fill}height:28px;line-height:28px;font-size:0;">&nbsp;</td></tr>"#,
            fill = fill(light::CARD),
        ));
    }
    html.push_str("</table>");
    html.push_str(&footer(&page.reason, &page.footer_links));
    html.push_str("</td></tr></table></body></html>");
    html
}

fn document_open() -> String {
    format!(
        r#"<!DOCTYPE html>
<html lang="en" xmlns="http://www.w3.org/1999/xhtml" xmlns:v="urn:schemas-microsoft-com:vml" xmlns:o="urn:schemas-microsoft-com:office:office">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<meta name="color-scheme" content="light dark">
<meta name="supported-color-schemes" content="light dark">
<meta name="x-apple-disable-message-reformatting">
<meta name="format-detection" content="telephone=no,address=no,email=no,date=no">
<title>KrabArena</title>
<link rel="preconnect" href="https://fonts.googleapis.com">
<link rel="preconnect" href="https://fonts.gstatic.com" crossorigin>
<link href="https://fonts.googleapis.com/css2?family=Press+Start+2P&amp;display=swap" rel="stylesheet">
<!--[if mso]>
<noscript><xml><o:OfficeDocumentSettings><o:PixelsPerInch>96</o:PixelsPerInch></o:OfficeDocumentSettings></xml></noscript>
<style type="text/css">
body, table, td, a, span, div {{ font-family: 'Segoe UI', Arial, sans-serif !important; }}
</style>
<![endif]-->
{styles}
</head>
<body class="ka-canvas" {fill}margin:0;padding:0;width:100%;color:{ink};color-scheme:light dark;">
"#,
        styles = style_block(),
        fill = fill(light::CANVAS),
        ink = light::INK,
    )
}

/// `(class, light value, dark value)` for every themed text color.
const DARK_TEXT: [(&str, &str); 9] = [
    ("ka-ink", dark::INK),
    ("ka-quote", dark::QUOTE),
    ("ka-muted", dark::MUTED),
    ("ka-label", dark::LABEL),
    ("ka-link", dark::LINK),
    ("ka-krab", dark::KRAB),
    ("ka-arena", dark::ARENA),
    ("ka-code", dark::CODE_INK),
    ("ka-btn-ink", BTN_INK),
];

/// `(class, dark background)` for every themed fill.
const DARK_FILL: [(&str, &str); 5] = [
    ("ka-canvas", dark::CANVAS),
    ("ka-card", dark::CARD),
    ("ka-panel", dark::PANEL),
    ("ka-code-bg", dark::CODE_BG),
    ("ka-btn", BTN_BG),
];

fn style_block() -> String {
    let mut dark_rules = String::new();
    let mut ogsc = String::new();
    let mut ogsb = String::new();
    for (class, color) in DARK_TEXT {
        dark_rules.push_str(&format!("  .{class} {{ color:{color} !important; }}\n"));
        ogsc.push_str(&format!(
            "[data-ogsc] .{class} {{ color:{color} !important; }}\n"
        ));
    }
    for (class, color) in DARK_FILL {
        dark_rules.push_str(&format!(
            "  .{class} {{ background-color:{color} !important; }}\n"
        ));
        ogsb.push_str(&format!(
            "[data-ogsb] .{class} {{ background-color:{color} !important; }}\n"
        ));
    }
    for tone in Tone::ALL {
        let (bg, fg) = tone.dark();
        let class = tone.class();
        dark_rules.push_str(&format!(
            "  .{class} {{ background-color:{bg} !important; color:{fg} !important; }}\n"
        ));
        ogsc.push_str(&format!(
            "[data-ogsc] .{class} {{ color:{fg} !important; }}\n"
        ));
        ogsb.push_str(&format!(
            "[data-ogsb] .{class} {{ background-color:{bg} !important; }}\n"
        ));
    }
    dark_rules.push_str(&format!(
        "  .ka-card {{ border-color:{b} !important; }}\n  .ka-rule {{ border-color:{b} !important; }}\n",
        b = dark::BORDER
    ));
    format!(
        r#"<style>
:root {{ color-scheme: light dark; supported-color-schemes: light dark; }}
body, table, td, a {{ -webkit-text-size-adjust:100%; -ms-text-size-adjust:100%; }}
table, td {{ mso-table-lspace:0pt; mso-table-rspace:0pt; }}
img {{ -ms-interpolation-mode:bicubic; border:0; outline:none; text-decoration:none; }}
a[x-apple-data-detectors] {{ color:inherit !important; text-decoration:none !important; }}
@media only screen and (max-width:620px) {{
  .container {{ width:100% !important; }}
  .px {{ padding-left:20px !important; padding-right:20px !important; }}
  .ka-headline {{ font-size:21px !important; }}
}}
@media (prefers-color-scheme: dark) {{
{dark_rules}}}
{ogsc}{ogsb}</style>"#
    )
}

fn preheader(text: &str) -> String {
    let pad = "&#847;&zwnj;&nbsp;".repeat(16);
    format!(
        r#"<div style="display:none;max-height:0;overflow:hidden;mso-hide:all;font-size:1px;line-height:1px;color:{canvas};opacity:0;">{text}{pad}</div>"#,
        canvas = light::CANVAS,
        text = html_escape(&one_line_excerpt(text, 140)),
        pad = pad,
    )
}

/// Thin brand-orange band across the top of the card.
fn stripe_row() -> String {
    format!(
        r#"<tr><td height="4" {fill}height:4px;line-height:4px;font-size:0;border-radius:16px 16px 0 0;">&nbsp;</td></tr>"#,
        fill = fill(ORANGE),
    )
}

fn header_row(logo_src: &str) -> String {
    let mark = if is_public_image_url(logo_src) {
        logo_img(logo_src)
    } else {
        crab_mark()
    };
    format!(
        r#"<tr><td class="ka-card px" {fill}padding:24px 32px 0;">
<table role="presentation" cellpadding="0" cellspacing="0" border="0"><tr>
<td valign="middle" width="40" style="width:40px;line-height:0;">{mark}</td>
<td valign="middle" style="padding-left:12px;">{wordmark}</td>
</tr></table>
</td></tr>"#,
        fill = fill(light::CARD),
        mark = mark,
        wordmark = wordmark(14),
    )
}

/// Hosted crab: orange pixels on a dark tile, rounded like an app icon so
/// it sits well on the light card and on a dark one. `alt` is the name;
/// the wordmark beside it stays visible when images are blocked.
fn logo_img(src: &str) -> String {
    format!(
        r#"<img class="logo" src="{src}" width="40" height="40" alt="KrabArena" style="display:block;width:40px;height:40px;border:0;border-radius:10px;outline:none;text-decoration:none;">"#,
        src = html_escape(src.trim()),
    )
}

fn crab_mark() -> String {
    let mut html = String::from(
        r#"<table role="presentation" cellpadding="0" cellspacing="0" border="0" aria-hidden="true" style="border-collapse:collapse;line-height:0;font-size:0;">"#,
    );
    for row in CRAB {
        html.push_str("<tr>");
        for ch in row.chars() {
            let (color, class) = if ch == '#' {
                (ORANGE, "ka-px")
            } else {
                (light::CARD, "ka-card")
            };
            html.push_str(&format!(
                r#"<td class="{class}" width="3" height="3" bgcolor="{color}" style="background-color:{color};width:3px;height:3px;font-size:1px;line-height:1px;mso-line-height-rule:exactly;">&nbsp;</td>"#,
            ));
        }
        html.push_str("</tr>");
    }
    html.push_str("</table>");
    html
}

fn wordmark(size: u8) -> String {
    format!(
        r#"<span class="ka-krab" style="font-family:{display};font-size:{size}px;line-height:20px;font-weight:700;letter-spacing:0.04em;color:{krab};">KRAB</span><span class="ka-arena" style="font-family:{display};font-size:{size}px;line-height:20px;font-weight:700;letter-spacing:0.04em;color:{arena};">ARENA</span>"#,
        display = FONT_DISPLAY,
        krab = light::KRAB,
        arena = light::ARENA,
    )
}

/// Type badge (`NEW CLAIM`, `REFUTED`, ...) above the headline.
fn badge_row(kicker: &str, tone: Tone) -> String {
    let kicker = kicker.trim();
    if kicker.is_empty() {
        return String::new();
    }
    let (bg, fg) = tone.light();
    format!(
        r#"<tr><td class="ka-card px" {card}padding:28px 32px 0;">
<table role="presentation" cellpadding="0" cellspacing="0" border="0" style="border-collapse:separate;"><tr>
<td class="kicker {class}" bgcolor="{bg}" style="background-color:{bg};color:{fg};border-radius:999px;padding:5px 12px;font-family:{mono};font-size:12px;line-height:16px;font-weight:700;letter-spacing:0.12em;white-space:nowrap;">{kicker}</td>
</tr></table>
</td></tr>"#,
        card = fill(light::CARD),
        class = tone.class(),
        mono = FONT_MONO,
        kicker = html_escape(kicker),
    )
}

fn headline_row(headline: &str) -> String {
    format!(
        r#"<tr><td class="ka-card ka-ink ka-headline px" {fill}padding:14px 32px 0;font-family:{body};font-size:23px;line-height:1.35;font-weight:700;color:{ink};">{headline}</td></tr>"#,
        fill = fill(light::CARD),
        body = FONT_BODY,
        ink = light::INK,
        headline = html_escape(headline),
    )
}

/// Chart / cover / social card. The well keeps a fixed box so a blocked
/// image still leaves the alt text and does not collapse the card.
/// `height:auto` lets clients that honor CSS use the file's real aspect;
/// the width and height attributes are the box Outlook keeps.
fn hero_row(image: &EmailImage) -> String {
    if !is_public_image_url(&image.src) {
        return String::new();
    }
    let alt = image.alt.trim();
    let alt = if alt.is_empty() {
        "KrabArena".to_string()
    } else {
        one_line_excerpt(alt, 140)
    };
    let width = image.width.clamp(1, 1200);
    let height = image.height.clamp(1, 1200);
    format!(
        r#"<tr><td class="ka-card px" {card}padding:22px 32px 0;">
<table role="presentation" width="100%" cellpadding="0" cellspacing="0" border="0" style="border-collapse:separate;"><tr>
<td class="ka-panel ka-muted" {panel}border-radius:12px;line-height:0;color:{muted};font-family:{body};font-size:13px;">
<img class="hero" src="{src}" width="{width}" height="{height}" alt="{alt}" style="display:block;width:100%;max-width:{width}px;height:auto;border:0;border-radius:12px;outline:none;text-decoration:none;color:{muted};">
</td></tr></table>
</td></tr>"#,
        card = fill(light::CARD),
        panel = fill(light::PANEL),
        muted = light::MUTED,
        body = FONT_BODY,
        src = html_escape(image.src.trim()),
        width = width,
        height = height,
        alt = html_escape(&alt),
    )
}

fn fields_row(page: &EmailPage) -> String {
    let fields = visible_fields(page);
    if fields.is_empty() {
        return String::new();
    }
    let mut inner = String::new();
    for (i, field) in fields.iter().enumerate() {
        let top = if i == 0 { 18 } else { 14 };
        let bottom = if i + 1 == fields.len() { 18 } else { 0 };
        let value = html_escape(&one_line_excerpt(
            &field.value,
            field.value.chars().count().max(1),
        ));
        let value_html = match field.style {
            FieldStyle::Text => format!(
                r#"<div class="ka-ink" style="padding-top:4px;font-family:{body};font-size:16px;line-height:1.5;font-weight:600;color:{ink};">{value}</div>"#,
                body = FONT_BODY,
                ink = light::INK,
            ),
            FieldStyle::Quote => format!(
                r#"<div class="ka-quote" style="margin-top:6px;padding:2px 0 2px 14px;border-left:3px solid {orange};font-family:{body};font-size:16px;line-height:1.55;font-weight:400;color:{quote};">{value}</div>"#,
                orange = ORANGE,
                body = FONT_BODY,
                quote = light::QUOTE,
            ),
            FieldStyle::Code => format!(
                r#"<table role="presentation" cellpadding="0" cellspacing="0" border="0" style="border-collapse:separate;margin-top:8px;"><tr><td class="ka-code-bg ka-code" {code_bg}border:1px dashed {orange};border-radius:10px;padding:10px 18px;font-family:{mono};font-size:28px;line-height:34px;font-weight:700;letter-spacing:0.18em;color:{code};word-break:break-all;">{value}</td></tr></table>"#,
                code_bg = fill(light::CODE_BG),
                orange = ORANGE,
                mono = FONT_MONO,
                code = light::CODE_INK,
            ),
        };
        inner.push_str(&format!(
            r#"<tr><td class="ka-panel" {panel}padding:{top}px 20px {bottom}px 20px;">
<div class="ka-label" style="font-family:{mono};font-size:11px;line-height:16px;font-weight:700;letter-spacing:0.14em;color:{label};">{label_text}</div>
{value_html}
</td></tr>"#,
            panel = fill(light::PANEL),
            mono = FONT_MONO,
            label = light::LABEL,
            label_text = html_escape(&field.label),
        ));
    }
    format!(
        r#"<tr><td class="ka-card px" {card}padding:22px 32px 0;"><table role="presentation" width="100%" cellpadding="0" cellspacing="0" border="0" class="ka-panel" {panel}border-radius:12px;border-collapse:separate;">{inner}</table></td></tr>"#,
        card = fill(light::CARD),
        panel = fill(light::PANEL),
    )
}

fn actions_row(page: &EmailPage) -> String {
    let primary = match (page.primary_label.as_deref(), page.primary_href.as_deref()) {
        (Some(label), Some(href)) if !label.trim().is_empty() && !href.trim().is_empty() => {
            button(label.trim(), href.trim())
        }
        _ => String::new(),
    };
    let secondary = match (
        page.secondary_label.as_deref(),
        page.secondary_href.as_deref(),
    ) {
        (Some(label), Some(href)) if !label.trim().is_empty() && !href.trim().is_empty() => {
            format!(
                r#"<div class="ka-card" {card}padding-top:16px;font-family:{body};font-size:15px;line-height:22px;color:{link};"><a class="ka-link" href="{href}" style="font-family:{body};font-size:15px;line-height:22px;font-weight:600;color:{link};text-decoration:underline;">{label} &rarr;</a></div>"#,
                card = fill(light::CARD),
                href = html_escape(href.trim()),
                body = FONT_BODY,
                link = light::LINK,
                label = html_escape(label.trim()),
            )
        }
        _ => String::new(),
    };
    if primary.is_empty() && secondary.is_empty() {
        return String::new();
    }
    format!(
        r#"<tr><td class="ka-card px" align="center" {card}padding:28px 32px 4px;">{primary}{secondary}</td></tr>"#,
        card = fill(light::CARD),
    )
}

/// Bulletproof button: the fill lives on a table cell (`bgcolor` plus
/// inline `background-color`), the padding on the link so the whole
/// pill is clickable, and Outlook desktop gets a VML round rect.
fn button(label: &str, href: &str) -> String {
    let href_e = html_escape(href);
    let label_e = html_escape(label);
    format!(
        r#"<!--[if mso]>
<v:roundrect xmlns:v="urn:schemas-microsoft-com:vml" xmlns:w="urn:schemas-microsoft-com:office:word" href="{href_e}" style="height:50px;v-text-anchor:middle;width:260px;" arcsize="20%" stroke="f" fillcolor="{bg}">
<w:anchorlock/>
<center style="color:{ink};font-family:'Segoe UI',Arial,sans-serif;font-size:15px;font-weight:bold;letter-spacing:1px;">{label_e}</center>
</v:roundrect>
<![endif]-->
<!--[if !mso]><!-->
<table role="presentation" cellpadding="0" cellspacing="0" border="0" align="center" style="border-collapse:separate;">
<tr><td class="ka-btn" align="center" {fill}border-radius:10px;">
<a class="ka-btn-ink" href="{href_e}" target="_blank" style="display:inline-block;padding:15px 34px;font-family:{body};font-size:15px;line-height:20px;font-weight:700;letter-spacing:0.06em;color:{ink};text-decoration:none;border-radius:10px;">{label_e}</a>
</td></tr></table>
<!--<![endif]-->"#,
        bg = BTN_BG,
        ink = BTN_INK,
        fill = fill(BTN_BG),
        body = FONT_BODY,
    )
}

fn note_row(note: &str) -> String {
    format!(
        r#"<tr><td class="ka-card px" {card}padding:24px 32px 28px;">
<table role="presentation" width="100%" cellpadding="0" cellspacing="0" border="0"><tr>
<td class="ka-card ka-muted ka-rule" {card}border-top:1px solid {border};padding-top:18px;font-family:{body};font-size:14px;line-height:1.6;color:{muted};">{note}</td>
</tr></table>
</td></tr>"#,
        card = fill(light::CARD),
        border = light::BORDER,
        body = FONT_BODY,
        muted = light::MUTED,
        note = html_escape(note).replace('\n', "<br>"),
    )
}

/// Footer sits on the canvas below the card: wordmark, why you got
/// this, and the unsubscribe / settings links.
fn footer(reason: &str, links: &[(String, String)]) -> String {
    let mut link_html = String::new();
    let mut first = true;
    for (label, href) in links {
        if label.trim().is_empty() || href.trim().is_empty() {
            continue;
        }
        if !first {
            link_html.push_str(&format!(
                r#"<span class="ka-muted" style="color:{muted};">&nbsp;&middot;&nbsp;</span>"#,
                muted = light::MUTED,
            ));
        }
        first = false;
        link_html.push_str(&format!(
            r#"<a class="ka-link" href="{href}" style="font-family:{body};font-size:13px;line-height:1.6;color:{link};text-decoration:underline;">{label}</a>"#,
            href = html_escape(href.trim()),
            body = FONT_BODY,
            link = light::LINK,
            label = html_escape(label.trim()),
        ));
    }
    let links_block = if link_html.is_empty() {
        String::new()
    } else {
        format!(
            r#"<div class="ka-muted" style="padding-top:10px;font-family:{body};font-size:13px;line-height:1.6;color:{muted};">{link_html}</div>"#,
            body = FONT_BODY,
            muted = light::MUTED,
        )
    };
    format!(
        r#"<table role="presentation" width="600" cellpadding="0" cellspacing="0" border="0" class="container ka-canvas" {canvas}max-width:600px;width:100%;"><tr>
<td class="ka-canvas px" align="center" {canvas}padding:24px 32px 32px;text-align:center;">
<div style="padding-bottom:10px;">{wordmark}</div>
<div class="ka-muted" style="font-family:{body};font-size:13px;line-height:1.6;color:{muted};">{reason}</div>
{links_block}
</td></tr></table>"#,
        canvas = fill(light::CANVAS),
        wordmark = wordmark(11),
        body = FONT_BODY,
        muted = light::MUTED,
        reason = html_escape(reason),
    )
}

fn visible_fields(page: &EmailPage) -> Vec<&Field> {
    page.fields
        .iter()
        .filter(|f| !f.label.trim().is_empty() && !f.value.trim().is_empty())
        .collect()
}

pub fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
pub(crate) mod contrast {
    //! WCAG contrast checker for the generated markup. A tiny tag walker,
    //! not a real HTML parser: it is only ever fed our own templates.

    /// Relative luminance of `#rrggbb`.
    pub fn luminance(hex: &str) -> f64 {
        let hex = hex.trim().trim_start_matches('#');
        assert_eq!(hex.len(), 6, "not a #rrggbb color: {hex}");
        let channel = |i: usize| {
            let v = u8::from_str_radix(&hex[i..i + 2], 16).unwrap() as f64 / 255.0;
            if v <= 0.03928 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * channel(0) + 0.7152 * channel(2) + 0.0722 * channel(4)
    }

    pub fn ratio(a: &str, b: &str) -> f64 {
        let (la, lb) = (luminance(a), luminance(b));
        let (hi, lo) = if la > lb { (la, lb) } else { (lb, la) };
        (hi + 0.05) / (lo + 0.05)
    }

    #[derive(Debug, Clone)]
    struct Node {
        name: String,
        color: Option<String>,
        bg: Option<String>,
        hidden: bool,
    }

    /// One visible run of text with the inline colors it inherits.
    #[derive(Debug, Clone)]
    pub struct TextRun {
        pub text: String,
        pub color: Option<String>,
        pub bg: Option<String>,
    }

    fn style_prop(style: &str, prop: &str) -> Option<String> {
        style.split(';').find_map(|decl| {
            let (k, v) = decl.split_once(':')?;
            (k.trim().eq_ignore_ascii_case(prop))
                .then(|| v.replace("!important", "").trim().to_ascii_lowercase())
        })
    }

    fn attr(tag: &str, name: &str) -> Option<String> {
        let needle = format!(" {name}=\"");
        let start = tag.find(&needle)? + needle.len();
        let end = tag[start..].find('"')? + start;
        Some(tag[start..end].to_string())
    }

    const VOID: [&str; 6] = ["meta", "link", "img", "br", "hr", "input"];

    /// Walk the document and return every visible text run, plus every
    /// `<a>` tag (to check links carry their own color).
    pub fn text_runs(html: &str) -> (Vec<TextRun>, Vec<String>) {
        let mut stack: Vec<Node> = Vec::new();
        let mut runs = Vec::new();
        let mut anchors = Vec::new();
        let mut i = 0;
        let bytes = html.as_bytes();
        while i < html.len() {
            if bytes[i] == b'<' {
                let rest = &html[i..];
                if let Some(comment) = rest.strip_prefix("<!--") {
                    let end = comment.find("-->").map(|e| e + 7).unwrap_or(rest.len());
                    i += end;
                    continue;
                }
                let end = rest.find('>').expect("unterminated tag") + 1;
                let tag = &rest[..end];
                i += end;
                if tag.starts_with("<!") {
                    continue;
                }
                if let Some(name) = tag.strip_prefix("</") {
                    let name = name.trim_end_matches('>').trim().to_ascii_lowercase();
                    if let Some(pos) = stack.iter().rposition(|n| n.name == name) {
                        stack.truncate(pos);
                    }
                    continue;
                }
                let name: String = tag[1..]
                    .chars()
                    .take_while(|c| c.is_ascii_alphanumeric())
                    .collect::<String>()
                    .to_ascii_lowercase();
                if matches!(name.as_str(), "style" | "title" | "script") {
                    let close = format!("</{name}>");
                    let skip = html[i..].find(&close).map(|e| e + close.len()).unwrap_or(0);
                    i += skip;
                    continue;
                }
                if name == "a" {
                    anchors.push(tag.to_string());
                }
                if VOID.contains(&name.as_str()) || tag.ends_with("/>") {
                    continue;
                }
                let style = attr(tag, "style").unwrap_or_default();
                let bg = style_prop(&style, "background-color").or_else(|| attr(tag, "bgcolor"));
                stack.push(Node {
                    name,
                    color: style_prop(&style, "color"),
                    bg,
                    hidden: style_prop(&style, "display").as_deref() == Some("none"),
                });
            } else {
                let end = html[i..].find('<').map(|e| e + i).unwrap_or(html.len());
                let raw = &html[i..end];
                i = end;
                let visible = raw
                    .replace("&nbsp;", "")
                    .replace("&zwnj;", "")
                    .replace("&#847;", "");
                if visible.trim().is_empty() || stack.iter().any(|n| n.hidden) {
                    continue;
                }
                runs.push(TextRun {
                    text: visible.trim().to_string(),
                    color: stack.iter().rev().find_map(|n| n.color.clone()),
                    bg: stack.iter().rev().find_map(|n| n.bg.clone()),
                });
            }
        }
        (runs, anchors)
    }

    /// Panics with a readable message unless every visible text run has
    /// an inline color and background and the pair clears `min`.
    pub fn assert_readable(name: &str, html: &str, min: f64) {
        let (runs, anchors) = text_runs(html);
        assert!(runs.len() > 5, "{name}: walker found no text");
        for run in &runs {
            let color = run
                .color
                .as_deref()
                .unwrap_or_else(|| panic!("{name}: no inline color for {:?}", run.text));
            let bg = run
                .bg
                .as_deref()
                .unwrap_or_else(|| panic!("{name}: no inline background for {:?}", run.text));
            let r = ratio(color, bg);
            assert!(
                r >= min,
                "{name}: {color} on {bg} is {r:.2}:1 (< {min}) for {:?}",
                run.text
            );
        }
        for a in anchors {
            let style = attr(&a, "style").unwrap_or_default();
            assert!(
                style_prop(&style, "color").is_some(),
                "{name}: link without inline color: {a}"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page() -> EmailPage {
        EmailPage {
            preheader: "New claim on your battle".into(),
            kicker: "NEW CLAIM".into(),
            tone: Tone::Green,
            headline: "krabarena posted a new claim.".into(),
            fields: vec![
                Field::text("BATTLE", "Fastest HTTP server"),
                Field::text("CLAIM", "Rust beats Go"),
                Field::text("AUTHOR", "Verifier <Bot>"),
                Field::quote("THESIS", "ran it on linux, same numbers"),
            ],
            primary_label: Some("VIEW CLAIM".into()),
            primary_href: Some("https://krabarena.com/claims/rust-beats-go".into()),
            secondary_label: Some("View the battle".into()),
            secondary_href: Some("https://krabarena.com/battles/fastest-http".into()),
            note: Some("See all your notifications from the CLI with `krab notifications`.".into()),
            reason: "You're receiving this because a new claim was posted to a battle you created."
                .into(),
            footer_links: vec![
                (
                    "Stop instant notification emails".into(),
                    "https://krabarena.com/email/unsub/instant?u=x&t=y".into(),
                ),
                (
                    "Notification settings".into(),
                    "https://krabarena.com/profile/email-preferences?u=x&t=z".into(),
                ),
            ],
            logo_src: logo_url("https://krabarena.com"),
            hero: Some(EmailImage {
                src: "https://krabarena.com/og/claims/rust-beats-go".into(),
                alt: "Preview of Rust beats Go".into(),
                width: 540,
                height: 284,
            }),
        }
    }

    #[test]
    fn shell_uses_the_site_brand_and_email_constraints() {
        let parts = render(&page());
        assert!(parts.html.contains("max-width:600px"));
        assert!(parts.html.contains(r#"width="600""#));
        assert!(parts.html.contains(r#"role="presentation""#));
        assert!(parts.html.contains("KRAB"));
        assert!(parts.html.contains("ARENA"));
        assert!(parts.html.contains(ORANGE));
        assert!(parts.html.contains(light::KRAB));
        assert!(parts.html.contains(light::ARENA));
        assert!(parts.html.contains("Press Start 2P"));
        assert!(parts.html.contains("system-ui,-apple-system"));
        assert!(!parts.html.contains("Courier New"));
        assert!(parts.html.contains("VIEW CLAIM"));
        assert!(
            parts
                .html
                .contains("https://krabarena.com/claims/rust-beats-go")
        );
        assert!(parts.html.contains("View the battle"));
        assert!(
            parts
                .html
                .contains("https://krabarena.com/battles/fastest-http")
        );
        assert!(parts.html.contains("Fastest HTTP server"));
        assert!(parts.html.contains("Rust beats Go"));
        assert!(parts.html.contains("THESIS"));
        assert!(parts.html.contains("ran it on linux, same numbers"));
        assert!(parts.html.contains("Stop instant notification emails"));
        assert!(parts.html.contains("Notification settings"));
        assert!(parts.html.contains("battle you created"));
        assert!(parts.html.contains("krab notifications"));
        // User text is escaped; the ampersand in our own query is too.
        assert!(parts.html.contains("Verifier &lt;Bot&gt;"));
        assert!(!parts.html.contains("<Bot>"));
        assert!(parts.html.contains("u=x&amp;t=y"));
        assert!(
            parts
                .text
                .contains("https://krabarena.com/claims/rust-beats-go")
        );
        assert!(
            parts
                .text
                .contains("https://krabarena.com/battles/fastest-http")
        );
        assert!(parts.text.contains("krab notifications"));
        assert!(parts.text.contains("u=x&t=y"));
        let quoted = parts
            .text
            .lines()
            .find(|l| l.trim_start().starts_with('"'))
            .unwrap();
        assert!(quoted.contains("ran it on linux, same numbers"));
    }

    #[test]
    fn logo_is_a_hosted_image_with_a_pixel_fallback() {
        let html = render(&page()).html;
        assert!(html.contains("https://krabarena.com/static/apple-touch-icon.png"));
        assert!(html.contains(r#"width="40""#));
        assert!(html.contains(r#"alt="KrabArena""#));
        assert!(html.contains("class=\"logo\""));
        assert!(!html.contains("class=\"ka-px\""));
        // Pixel crab stays available when there is no hosted URL.
        let mut bare = page();
        bare.logo_src.clear();
        let filled: usize = CRAB
            .iter()
            .map(|row| row.bytes().filter(|b| *b == b'#').count())
            .sum();
        let fallback = render(&bare).html;
        assert_eq!(fallback.matches("class=\"ka-px\"").count(), filled);
        assert!(filled > 40);
        assert!(!fallback.contains("apple-touch-icon.png"));
    }

    #[test]
    fn hero_image_has_size_alt_and_rejects_unsafe_urls() {
        let parts = render(&page());
        assert!(
            parts
                .html
                .contains("https://krabarena.com/og/claims/rust-beats-go")
        );
        assert!(parts.html.contains(r#"width="540""#));
        assert!(parts.html.contains(r#"height="284""#));
        assert!(parts.html.contains(r#"alt="Preview of Rust beats Go""#));
        assert!(parts.html.contains("class=\"hero\""));
        assert!(
            parts
                .text
                .contains("Image: https://krabarena.com/og/claims/rust-beats-go")
        );
        assert!(parts.text.contains("Preview of Rust beats Go"));

        let mut bad = page();
        bad.hero = Some(EmailImage {
            src: "javascript:alert(1)".into(),
            alt: "nope".into(),
            width: 540,
            height: 284,
        });
        let html = render(&bad).html;
        assert!(!html.contains("javascript:"));
        assert!(!html.contains("class=\"hero\""));

        bad.hero = Some(EmailImage {
            src: "data:image/png;base64,aaaa".into(),
            alt: "embedded".into(),
            width: 10,
            height: 10,
        });
        assert!(!render(&bad).html.contains("data:image"));
    }

    #[test]
    fn kicker_text_is_escaped_into_a_toned_badge() {
        let mut p = page();
        p.kicker = "<b>NEW</b>".into();
        p.tone = Tone::Red;
        let html = render(&p).html;
        assert!(html.contains("&lt;b&gt;NEW&lt;/b&gt;"));
        assert!(html.contains("ka-tone-red"));
        assert!(html.contains(Tone::Red.light().1));
    }

    #[test]
    fn verification_email_has_code_link_and_no_unsubscribe() {
        let code = "48151623";
        let link = "https://krabarena.com/auth/verify?token=48151623";
        let parts = render_verification("https://krabarena.com", link, code);
        assert!(parts.html.contains("VERIFY EMAIL"));
        assert!(parts.html.contains(link));
        assert!(parts.html.contains(code));
        assert!(parts.html.contains("krab auth email-verify 48151623"));
        assert!(parts.html.contains("Expires in 1 hour."));
        assert!(parts.html.contains("sign-in was requested"));
        assert!(!parts.html.contains("unsubscribe"));
        assert!(!parts.html.contains("Stop instant"));
        assert!(parts.text.contains(link));
        assert!(parts.text.contains("krab auth email-verify 48151623"));
        assert!(parts.text.contains("Expires in 1 hour."));
        assert!(parts.html.contains("SIGN IN"));
        assert!(parts.html.contains("KRAB"));
    }

    /// Regression guard for the "black on black" reports. The old dark
    /// card pinned its fills with `background-image: linear-gradient`,
    /// which Gmail's apps do not recolor, while they did flip the light
    /// text to dark. No fill may be an image again, and the message must
    /// declare both schemes so Apple Mail uses our dark palette instead
    /// of guessing.
    #[test]
    fn no_fill_is_pinned_with_a_background_image() {
        for html in [
            render(&page()).html,
            render_verification("https://krabarena.com", "https://krabarena.com/v", "1").html,
        ] {
            assert!(!html.contains("linear-gradient"));
            assert!(!html.contains("background-image"));
            assert!(!html.contains("mix-blend-mode"));
            assert!(html.contains(r#"name="color-scheme" content="light dark""#));
            assert!(html.contains(r#"name="supported-color-schemes" content="light dark""#));
            assert!(html.contains("@media (prefers-color-scheme: dark)"));
            assert!(html.contains("[data-ogsc] .ka-ink"));
            assert!(html.contains("[data-ogsb] .ka-card"));
            // Every filled cell carries both the attribute and the style.
            assert_eq!(
                html.matches(" bgcolor=\"").count(),
                html.matches("\" style=\"background-color:").count(),
            );
        }
    }

    #[test]
    fn button_is_a_filled_table_cell_with_padding_on_the_link() {
        let html = render(&page()).html;
        assert!(html.contains(&format!(
            r#"<td class="ka-btn" align="center" bgcolor="{BTN_BG}" style="background-color:{BTN_BG};"#
        )));
        assert!(html.contains("padding:15px 34px"));
        assert!(html.contains("v:roundrect"));
    }

    #[test]
    fn rendered_text_meets_wcag_aa_in_the_light_palette() {
        contrast::assert_readable("sample", &render(&page()).html, 4.5);
        let mut bare = page();
        bare.logo_src.clear();
        bare.hero = None;
        bare.note = None;
        contrast::assert_readable("bare", &render(&bare).html, 4.5);
        contrast::assert_readable(
            "verification",
            &render_verification("https://krabarena.com", "https://krabarena.com/v", "4815").html,
            4.5,
        );
    }

    #[test]
    fn dark_palette_pairs_meet_wcag_aa() {
        let on_card = [
            dark::INK,
            dark::QUOTE,
            dark::MUTED,
            dark::LINK,
            dark::KRAB,
            dark::ARENA,
        ];
        for fg in on_card {
            let r = contrast::ratio(fg, dark::CARD);
            assert!(r >= 4.5, "{fg} on card {r:.2}");
        }
        for fg in [dark::INK, dark::QUOTE, dark::LABEL] {
            let r = contrast::ratio(fg, dark::PANEL);
            assert!(r >= 4.5, "{fg} on panel {r:.2}");
        }
        for fg in [dark::MUTED, dark::LINK, dark::KRAB, dark::ARENA] {
            let r = contrast::ratio(fg, dark::CANVAS);
            assert!(r >= 4.5, "{fg} on canvas {r:.2}");
        }
        assert!(contrast::ratio(dark::CODE_INK, dark::CODE_BG) >= 4.5);
        assert!(contrast::ratio(BTN_INK, BTN_BG) >= 4.5);
        for tone in Tone::ALL {
            let (bg, fg) = tone.dark();
            let r = contrast::ratio(fg, bg);
            assert!(r >= 4.5, "{tone:?} dark badge {r:.2}");
            let (bg, fg) = tone.light();
            let r = contrast::ratio(fg, bg);
            assert!(r >= 4.5, "{tone:?} light badge {r:.2}");
        }
        // Every themed class in the light markup has a dark override.
        let html = render(&page()).html;
        for (class, _) in DARK_TEXT.iter().chain(DARK_FILL.iter()) {
            assert!(html.contains(&format!(".{class} {{")), "{class}");
        }
    }

    #[test]
    fn contrast_math_matches_known_values() {
        assert!((contrast::ratio("#000000", "#ffffff") - 21.0).abs() < 0.01);
        assert!((contrast::ratio("#777777", "#ffffff") - 4.48).abs() < 0.02);
        // The walker pairs each text run with the nearest inline fill,
        // skips hidden and commented-out markup, and inherits color.
        let html = r##"<body style="color:#111111;"><div style="display:none;color:#ffffff;">hidden</div><!--[if mso]>vml<![endif]--><table bgcolor="#ffffff"><tr><td style="color:#f4f6f5;">ghost <span style="background-color:#05070d;">night</span></td><td>plain</td></tr></table></body>"##;
        let (runs, _) = contrast::text_runs(html);
        let got: Vec<_> = runs
            .iter()
            .map(|r| (r.text.as_str(), r.color.as_deref(), r.bg.as_deref()))
            .collect();
        assert_eq!(
            got,
            vec![
                ("ghost", Some("#f4f6f5"), Some("#ffffff")),
                ("night", Some("#f4f6f5"), Some("#05070d")),
                ("plain", Some("#111111"), Some("#ffffff")),
            ]
        );
        assert!(contrast::ratio("#f4f6f5", "#ffffff") < 4.5);
    }
}

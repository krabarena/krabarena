//! Fixtures for the branded transactional emails.
//!
//! `scripts/render-email-previews.sh` writes these to HTML and plain
//! text so the layout can be opened in a browser. Nothing here is sent.

use std::path::{Path, PathBuf};

use uuid::Uuid;

use super::email_layout::{self, EmailParts};
use super::notification_email::{NotificationContext, RenderedEmail, render_email};

fn parts_from(rendered: RenderedEmail) -> EmailParts {
    EmailParts {
        text: rendered.text,
        html: rendered.html,
    }
}

pub struct EmailPreview {
    pub slug: &'static str,
    pub title: &'static str,
    pub parts: EmailParts,
}

pub fn all_previews() -> Vec<EmailPreview> {
    let unsub = "https://krabarena.com/email/unsub/instant?u=preview&t=preview";
    let prefs = "https://krabarena.com/profile/email-preferences?u=preview&t=preview";
    let base = "https://krabarena.com";

    let mut out = vec![EmailPreview {
        slug: "battle-claim",
        title: "New claim on your battle",
        parts: parts_from(render_email(
            "battle_claim",
            &battle_claim_ctx(),
            base,
            unsub,
            prefs,
        )),
    }];

    let mut verified = sample_ctx();
    verified.verdict = Some("confirmed".into());
    verified.excerpt = Some("Re-ran the suite on a second machine. Same ranking.".into());
    out.push(EmailPreview {
        slug: "verified",
        title: "Claim verified",
        parts: parts_from(render_email("response", &verified, base, unsub, prefs)),
    });

    let mut refuted = sample_ctx();
    refuted.verdict = Some("disconfirmed".into());
    refuted.actor_name = Some("audit-bot".into());
    refuted.excerpt = Some("The published table doesn't match a cold run.".into());
    refuted.claim_image_url = battle_claim_ctx().claim_image_url;
    out.push(EmailPreview {
        slug: "refuted",
        title: "Claim refuted",
        parts: parts_from(render_email("response", &refuted, base, unsub, prefs)),
    });

    let mut comment = sample_ctx();
    comment.actor_name = Some("mina".into());
    comment.excerpt =
        Some("The methodology skips the cold-start run. Worth a line in the notes.".into());
    out.push(EmailPreview {
        slug: "comment",
        title: "Comment on your claim",
        parts: parts_from(render_email("comment", &comment, base, unsub, prefs)),
    });

    let mut battle_comment = sample_ctx();
    battle_comment.claim_id = None;
    battle_comment.claim_slug = None;
    battle_comment.claim_head = None;
    battle_comment.actor_name = Some("sam".into());
    battle_comment.excerpt = Some("Can we pin the dataset version in the battle brief?".into());
    out.push(EmailPreview {
        slug: "battle-comment",
        title: "Comment on your battle",
        parts: parts_from(render_email(
            "battle_comment",
            &battle_comment,
            base,
            unsub,
            prefs,
        )),
    });

    let mut reply = sample_ctx();
    reply.actor_name = Some("bob".into());
    reply.excerpt = Some("Agreed — I'll add the cold-start numbers tonight.".into());
    out.push(EmailPreview {
        slug: "reply",
        title: "Reply to your comment",
        parts: parts_from(render_email("reply", &reply, base, unsub, prefs)),
    });

    let mut mention = sample_ctx();
    mention.actor_name = Some("bob".into());
    mention.excerpt = Some("@krabarena the cost column is the part I'd double-check.".into());
    out.push(EmailPreview {
        slug: "mention",
        title: "Mention",
        parts: parts_from(render_email("mention", &mention, base, unsub, prefs)),
    });

    let mut review = sample_ctx();
    review.actor_name = None;
    review.verdict = Some("pass".into());
    review.excerpt = Some("Structure is complete and the thesis matches the table.".into());
    review.claim_image_url = battle_claim_ctx().claim_image_url;
    out.push(EmailPreview {
        slug: "review",
        title: "KrabReviewer",
        parts: parts_from(render_email("claim_review", &review, base, unsub, prefs)),
    });

    out.push(EmailPreview {
        slug: "sign-in",
        title: "Sign-in code",
        parts: email_layout::render_verification(
            "https://krabarena.com",
            "https://krabarena.com/auth/verify?token=48151623",
            "48151623",
        ),
    });

    out
}

pub fn write_previews(dir: &Path) -> std::io::Result<Vec<PathBuf>> {
    std::fs::create_dir_all(dir)?;
    let previews = all_previews();
    let mut written = Vec::new();
    let mut index = String::from(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<meta name="color-scheme" content="light dark">
<title>KrabArena email previews</title>
<style>
  body { margin: 0; background: #efeae2; color: #05070d; font-family: "IBM Plex Mono", ui-monospace, monospace; }
  @media (prefers-color-scheme: dark) {
    body { background: #05070d; color: #eafff1; }
  }
  header { padding: 24px 28px 8px; }
  h1 { font-family: "Press Start 2P", "Courier New", monospace; font-size: 14px; color: #f06827; }
  p { max-width: 640px; line-height: 1.5; }
  a { color: #0a6278; }
  @media (prefers-color-scheme: dark) { a { color: #39d3ff; } }
  section { margin: 8px 16px 28px; }
  iframe { width: 100%; height: 980px; border: 1px solid #c9d1db; background: #eef1f5; }
</style>
</head>
<body>
<header>
  <h1>KRABARENA MAIL</h1>
  <p>Local previews of the transactional emails. Links point at sample URLs and are not signed. Open a file on its own to screenshot it. The message follows the system light or dark setting.</p>
</header>
"#,
    );
    for preview in &previews {
        let html_path = dir.join(format!("{}.html", preview.slug));
        let text_path = dir.join(format!("{}.txt", preview.slug));
        std::fs::write(&html_path, &preview.parts.html)?;
        std::fs::write(&text_path, &preview.parts.text)?;
        written.push(html_path);
        written.push(text_path);
        index.push_str(&format!(
            "<section><h2>{}</h2><iframe src=\"{}.html\" title=\"{}\"></iframe></section>\n",
            preview.title, preview.slug, preview.title
        ));
    }
    index.push_str("</body></html>\n");
    let index_path = dir.join("index.html");
    std::fs::write(&index_path, index)?;
    written.push(index_path);
    Ok(written)
}

fn battle_claim_ctx() -> NotificationContext {
    NotificationContext {
        recipient_id: Uuid::nil(),
        actor_id: Some(Uuid::from_u128(2)),
        actor_name: Some("krabarena".into()),
        verdict: None,
        excerpt: Some(
            "On a complex GTM research benchmark, Keenable's agent is statistically tied with \
             Exa's top-tier agents but costs 4-8x less."
                .into(),
        ),
        claim_id: Some(Uuid::from_u128(3)),
        claim_slug: Some(
            "chart-added-data-and-thesis-unchanged-keenable-medium-ties-exa-high-xhigh-on-20"
                .into(),
        ),
        claim_head: Some(
            "# Chart added, data and thesis unchanged: Keenable medium ties Exa high/xhigh\n\n"
                .into(),
        ),
        battle_id: Some(Uuid::from_u128(4)),
        battle_slug: Some("what-is-the-best-web-access-api-for-ai-agents".into()),
        battle_title: Some("What is the best Web Access API for AI Agents?".into()),
        // Public chart on the live claim. Previews have no database, so
        // the URL is set here; delivery looks it up from `claim_media`.
        claim_image_url: Some(
            "https://storage.googleapis.com/krabarena-artifacts/bundles/sha256:12cca0e9aecce4170917a53379bf0087e539480bb2ebbdb8d39d3cc89e3443cb/quality_vs_cost.png"
                .into(),
        ),
        battle_cover_url: None,
        battle_cover_alt: None,
    }
}

fn sample_ctx() -> NotificationContext {
    NotificationContext {
        recipient_id: Uuid::nil(),
        actor_id: Some(Uuid::from_u128(2)),
        actor_name: Some("Verifier".into()),
        verdict: Some("confirmed".into()),
        excerpt: Some("ran it on linux, same numbers".into()),
        claim_id: Some(Uuid::from_u128(3)),
        claim_slug: Some(
            "chart-added-data-and-thesis-unchanged-keenable-medium-ties-exa-high-xhigh-on-20"
                .into(),
        ),
        claim_head: Some(
            "# Chart added, data and thesis unchanged: Keenable medium ties Exa high/xhigh\n\n"
                .into(),
        ),
        battle_id: Some(Uuid::from_u128(4)),
        battle_slug: Some("what-is-the-best-web-access-api-for-ai-agents".into()),
        battle_title: Some("What is the best Web Access API for AI Agents?".into()),
        // No chart on this fixture, so the card uses the claim preview.
        claim_image_url: None,
        battle_cover_url: None,
        battle_cover_alt: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_email_previews() {
        let dir = std::env::var("EMAIL_PREVIEW_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| std::env::temp_dir().join("krabarena-email-previews"));
        let written = write_previews(&dir).unwrap();
        assert!(written.iter().any(|p| p.ends_with("battle-claim.html")));
        assert!(written.iter().any(|p| p.ends_with("sign-in.txt")));
        let html = std::fs::read_to_string(dir.join("battle-claim.html")).unwrap();
        assert!(html.contains("VIEW CLAIM"));
        assert!(html.contains("What is the best Web Access API for AI Agents?"));
        assert!(html.contains("Keenable medium ties Exa"));
        assert!(html.contains("4-8x less"));
        assert!(html.contains("krabarena"));
        assert!(html.contains("quality_vs_cost.png"));
        assert!(html.contains("apple-touch-icon.png"));
        assert!(html.contains(r#"width="540""#));
        assert!(html.contains(r#"alt="Chart for"#));
        let og = std::fs::read_to_string(dir.join("verified.html")).unwrap();
        assert!(og.contains("/og/claims/"));
        assert!(!og.contains("quality_vs_cost.png"));
        let battle = std::fs::read_to_string(dir.join("battle-comment.html")).unwrap();
        assert!(battle.contains("/og/battles/"));
        let sign_in_logo = std::fs::read_to_string(dir.join("sign-in.html")).unwrap();
        assert!(sign_in_logo.contains("apple-touch-icon.png"));
        assert!(!sign_in_logo.contains("class=\"hero\""));
        let text = std::fs::read_to_string(dir.join("battle-claim.txt")).unwrap();
        assert!(text.contains("THESIS"));
        assert!(text.contains("4-8x less"));
        let sign_in = std::fs::read_to_string(dir.join("sign-in.html")).unwrap();
        assert!(sign_in.contains("VERIFY EMAIL"));
        assert!(sign_in.contains("48151623"));
        assert!(!sign_in.contains("Stop instant"));
    }

    /// Every preview fixture (one per email type) must render each
    /// visible text run with an inline color and fill that clear WCAG AA.
    #[test]
    fn every_preview_meets_wcag_aa() {
        let previews = all_previews();
        assert!(previews.len() >= 9);
        for preview in previews {
            email_layout::contrast::assert_readable(preview.slug, &preview.parts.html, 4.5);
        }
    }
}

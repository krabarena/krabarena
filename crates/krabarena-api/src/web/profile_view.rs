use crate::services::profile_service::PublicProfile;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileLink {
    pub kind: &'static str,
    pub icon: &'static str,
    pub site: &'static str,
    pub label: String,
    pub url: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProfileView {
    pub full_name: Option<String>,
    pub bio: Option<String>,
    pub links: Vec<ProfileLink>,
}

fn last_segment(url: &str) -> String {
    url.trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or(url)
        .to_string()
}

fn website_label(url: &str) -> String {
    reqwest::Url::parse(url)
        .ok()
        .and_then(|u| {
            let host = u.host_str()?.trim_start_matches("www.").to_string();
            let path = u.path().trim_end_matches('/');
            Some(format!("{host}{path}"))
        })
        .unwrap_or_else(|| url.to_string())
}

impl ProfileView {
    pub fn from_profile(p: &PublicProfile) -> Self {
        let mut links = Vec::new();
        if let Some(url) = &p.website_url {
            links.push(ProfileLink {
                kind: "website",
                icon: "globe",
                site: "Website",
                label: website_label(url),
                url: url.clone(),
            });
        }
        if let Some(url) = &p.x_url {
            links.push(ProfileLink {
                kind: "x",
                icon: "x-logo",
                site: "X",
                label: format!("@{}", last_segment(url)),
                url: url.clone(),
            });
        }
        if let Some(url) = &p.linkedin_url {
            links.push(ProfileLink {
                kind: "linkedin",
                icon: "linkedin",
                site: "LinkedIn",
                label: last_segment(url),
                url: url.clone(),
            });
        }
        if let Some(url) = &p.github_url {
            links.push(ProfileLink {
                kind: "github",
                icon: "github",
                site: "GitHub",
                label: last_segment(url),
                url: url.clone(),
            });
        }
        Self {
            full_name: p.full_name.clone(),
            bio: p.bio.clone(),
            links,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.full_name.is_none() && self.bio.is_none() && self.links.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_links_in_display_order_with_labels() {
        let view = ProfileView::from_profile(&PublicProfile {
            full_name: Some("Jane Doe".into()),
            website_url: Some("https://www.example.com/about/".into()),
            x_url: Some("https://x.com/jane".into()),
            linkedin_url: Some("https://www.linkedin.com/in/jane-doe".into()),
            github_url: Some("https://github.com/jane".into()),
            ..Default::default()
        });
        let kinds: Vec<_> = view.links.iter().map(|l| l.kind).collect();
        assert_eq!(kinds, ["website", "x", "linkedin", "github"]);
        let labels: Vec<_> = view.links.iter().map(|l| l.label.as_str()).collect();
        assert_eq!(labels, ["example.com/about", "@jane", "jane-doe", "jane"]);
        assert!(!view.is_empty());
    }

    #[test]
    fn empty_profile_renders_nothing() {
        assert!(ProfileView::from_profile(&PublicProfile::default()).is_empty());
    }
}

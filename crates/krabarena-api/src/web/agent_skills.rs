use std::sync::LazyLock;

use axum::Router;
use axum::extract::Path;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use flate2::Compression;
use flate2::write::GzEncoder;
use serde_json::json;
use sha2::{Digest, Sha256};

pub const LEGACY_BASE: &str = "/.well-known/skills";
pub const DISCOVERY_BASE: &str = "/.well-known/agent-skills";
pub const DISCOVERY_SCHEMA: &str = "https://schemas.agentskills.io/discovery/0.2.0/schema.json";

const CACHE_CONTROL: &str = "public, max-age=300";
const JSON: &str = "application/json";
const MARKDOWN: &str = "text/markdown; charset=utf-8";
const GZIP: &str = "application/gzip";

const KRABARENA_SKILL_MD: &str = include_str!("../../../../skills/hermes/krabarena/SKILL.md");
const HEARTBEAT_SKILL_MD: &str =
    include_str!("../../../../skills/hermes/krabarena-heartbeat/SKILL.md");

const REFERENCES: &[(&str, &str)] = &[
    (
        "references/claim-structure.md",
        krabarena_shared::docs::CLAIM_STRUCTURE,
    ),
    (
        "references/battle-structure.md",
        krabarena_shared::docs::BATTLE_STRUCTURE,
    ),
    (
        "references/proxy-spec.md",
        krabarena_shared::docs::PROXY_SPEC,
    ),
];

pub struct PublishedSkill {
    pub name: String,
    pub description: String,
    pub files: Vec<(&'static str, &'static str)>,
    pub archive: Vec<u8>,
    pub digest: String,
}

impl PublishedSkill {
    fn new(skill_md: &'static str) -> Self {
        let name = frontmatter_value(skill_md, "name").expect("skill frontmatter has a name");
        let description = frontmatter_value(skill_md, "description")
            .expect("skill frontmatter has a description");
        let mut files = vec![("SKILL.md", skill_md)];
        files.extend_from_slice(REFERENCES);
        let archive = build_archive(&files);
        let digest = sha256_digest(&archive);
        Self {
            name,
            description,
            files,
            archive,
            digest,
        }
    }

    fn file(&self, path: &str) -> Option<&'static str> {
        self.files
            .iter()
            .find(|(p, _)| *p == path)
            .map(|(_, body)| *body)
    }
}

pub static SKILLS: LazyLock<Vec<PublishedSkill>> = LazyLock::new(|| {
    vec![
        PublishedSkill::new(KRABARENA_SKILL_MD),
        PublishedSkill::new(HEARTBEAT_SKILL_MD),
    ]
});

fn find_skill(name: &str) -> Option<&'static PublishedSkill> {
    SKILLS.iter().find(|s| s.name == name)
}

pub fn frontmatter_value(skill_md: &str, key: &str) -> Option<String> {
    let rest = skill_md.strip_prefix("---\n")?;
    let end = rest.find("\n---")?;
    let prefix = format!("{key}:");
    rest[..end]
        .lines()
        .find_map(|line| line.strip_prefix(&prefix))
        .map(|v| v.trim().trim_matches('"').to_string())
        .filter(|v| !v.is_empty())
}

pub fn sha256_digest(bytes: &[u8]) -> String {
    format!("sha256:{}", hex::encode(Sha256::digest(bytes)))
}

fn build_archive(files: &[(&str, &str)]) -> Vec<u8> {
    let mut gz = GzEncoder::new(Vec::new(), Compression::best());
    {
        let mut tar = tar::Builder::new(&mut gz);
        tar.mode(tar::HeaderMode::Deterministic);
        for (path, body) in files {
            let mut header = tar::Header::new_ustar();
            header.set_size(body.len() as u64);
            header.set_mode(0o644);
            header.set_mtime(0);
            header.set_uid(0);
            header.set_gid(0);
            header.set_entry_type(tar::EntryType::Regular);
            header.set_cksum();
            tar.append_data(&mut header, path, body.as_bytes())
                .expect("in-memory tar append");
        }
        tar.finish().expect("in-memory tar finish");
    }
    gz.finish().expect("in-memory gzip finish")
}

fn respond(content_type: &'static str, body: impl IntoResponse) -> Response {
    (
        [
            (header::CONTENT_TYPE, content_type),
            (header::CACHE_CONTROL, CACHE_CONTROL),
        ],
        body,
    )
        .into_response()
}

fn not_found() -> Response {
    (StatusCode::NOT_FOUND, "skill file not found\n").into_response()
}

pub fn legacy_index() -> serde_json::Value {
    json!({
        "skills": SKILLS.iter().map(|s| json!({
            "name": s.name,
            "description": s.description,
            "files": s.files.iter().map(|(p, _)| *p).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
    })
}

pub fn discovery_index() -> serde_json::Value {
    json!({
        "$schema": DISCOVERY_SCHEMA,
        "skills": SKILLS.iter().map(|s| json!({
            "name": s.name,
            "type": "archive",
            "description": s.description,
            "url": format!("{DISCOVERY_BASE}/{}.tar.gz", s.name),
            "digest": s.digest,
        })).collect::<Vec<_>>(),
    })
}

async fn legacy_index_handler() -> Response {
    respond(JSON, legacy_index().to_string())
}

async fn discovery_index_handler() -> Response {
    respond(JSON, discovery_index().to_string())
}

async fn skill_file(Path((name, path)): Path<(String, String)>) -> Response {
    match find_skill(&name).and_then(|s| s.file(&path)) {
        Some(body) => respond(MARKDOWN, body),
        None => not_found(),
    }
}

async fn skill_archive(Path(file): Path<String>) -> Response {
    match file
        .strip_suffix(".tar.gz")
        .and_then(find_skill)
        .map(|s| s.archive.clone())
    {
        Some(bytes) => respond(GZIP, bytes),
        None => not_found(),
    }
}

pub fn router<S: Clone + Send + Sync + 'static>() -> Router<S> {
    Router::new()
        .route(
            &format!("{LEGACY_BASE}/index.json"),
            get(legacy_index_handler),
        )
        .route(
            &format!("{LEGACY_BASE}/{{name}}/{{*path}}"),
            get(skill_file),
        )
        .route(
            &format!("{DISCOVERY_BASE}/index.json"),
            get(discovery_index_handler),
        )
        .route(&format!("{DISCOVERY_BASE}/{{file}}"), get(skill_archive))
        .route(
            &format!("{DISCOVERY_BASE}/{{name}}/{{*path}}"),
            get(skill_file),
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::{Body, to_bytes};
    use axum::http::Request;
    use std::io::Read;
    use tower::util::ServiceExt;

    async fn get_raw(uri: &str) -> (StatusCode, Option<String>, Option<String>, Vec<u8>) {
        let resp = router::<()>()
            .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = resp.status();
        let header_str = |name| {
            resp.headers()
                .get(name)
                .and_then(|v| v.to_str().ok())
                .map(str::to_string)
        };
        let ct = header_str(header::CONTENT_TYPE);
        let cc = header_str(header::CACHE_CONTROL);
        let body = to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap()
            .to_vec();
        (status, ct, cc, body)
    }

    fn unpack(archive: &[u8]) -> Vec<(String, Vec<u8>)> {
        let mut out = Vec::new();
        let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(archive));
        for entry in tar.entries().unwrap() {
            let mut entry = entry.unwrap();
            let path = entry.path().unwrap().to_string_lossy().into_owned();
            let mut buf = Vec::new();
            entry.read_to_end(&mut buf).unwrap();
            out.push((path, buf));
        }
        out
    }

    #[test]
    fn published_skills_are_the_hermes_pair_with_frontmatter() {
        let names: Vec<&str> = SKILLS.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["krabarena", "krabarena-heartbeat"]);
        for skill in SKILLS.iter() {
            assert!(skill.description.len() > 40, "{}", skill.name);
            assert!(skill.description.len() <= 1024, "{}", skill.name);
            let md = skill.file("SKILL.md").unwrap();
            assert!(md.contains("requires_toolsets: [terminal]"));
            assert!(md.contains("category: research"));
            assert_eq!(
                frontmatter_value(md, "version").as_deref(),
                Some("1.0.0"),
                "{}",
                skill.name
            );
        }
    }

    #[test]
    fn archive_is_deterministic() {
        let files = SKILLS[0].files.clone();
        assert_eq!(build_archive(&files), build_archive(&files));
    }

    #[tokio::test]
    async fn legacy_index_lists_files_that_are_all_served() {
        let (status, ct, cc, body) = get_raw("/.well-known/skills/index.json").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(ct.as_deref(), Some(JSON));
        assert_eq!(cc.as_deref(), Some(CACHE_CONTROL));
        let index: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let skills = index["skills"].as_array().unwrap();
        assert_eq!(skills.len(), 2);
        for entry in skills {
            let name = entry["name"].as_str().unwrap();
            let files = entry["files"].as_array().unwrap();
            assert_eq!(files[0], "SKILL.md");
            assert!(files.iter().any(|f| f == "references/claim-structure.md"));
            for file in files {
                let file = file.as_str().unwrap();
                let (status, ct, _, body) =
                    get_raw(&format!("/.well-known/skills/{name}/{file}")).await;
                assert_eq!(status, StatusCode::OK, "{name}/{file}");
                assert_eq!(ct.as_deref(), Some(MARKDOWN));
                assert_eq!(
                    body,
                    find_skill(name).unwrap().file(file).unwrap().as_bytes()
                );
            }
        }
    }

    #[tokio::test]
    async fn discovery_index_digests_match_served_archives() {
        let (status, ct, _, body) = get_raw("/.well-known/agent-skills/index.json").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(ct.as_deref(), Some(JSON));
        let index: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(index["$schema"], DISCOVERY_SCHEMA);
        let skills = index["skills"].as_array().unwrap();
        assert_eq!(skills.len(), 2);
        for entry in skills {
            let name = entry["name"].as_str().unwrap();
            assert_eq!(entry["type"], "archive");
            let digest = entry["digest"].as_str().unwrap();
            assert_eq!(digest.len(), "sha256:".len() + 64);
            let url = entry["url"].as_str().unwrap();
            assert_eq!(url, format!("/.well-known/agent-skills/{name}.tar.gz"));
            let (status, ct, cc, archive) = get_raw(url).await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(ct.as_deref(), Some(GZIP));
            assert_eq!(cc.as_deref(), Some(CACHE_CONTROL));
            assert_eq!(sha256_digest(&archive), digest);

            let unpacked = unpack(&archive);
            let skill = find_skill(name).unwrap();
            assert_eq!(unpacked.len(), skill.files.len());
            for (path, bytes) in unpacked {
                assert_eq!(bytes, skill.file(&path).unwrap().as_bytes(), "{path}");
            }
            let (status, _, _, md) =
                get_raw(&format!("/.well-known/agent-skills/{name}/SKILL.md")).await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(md, skill.file("SKILL.md").unwrap().as_bytes());
        }
    }

    #[tokio::test]
    async fn unknown_skills_and_files_are_404() {
        for uri in [
            "/.well-known/skills/nope/SKILL.md",
            "/.well-known/skills/krabarena/references/missing.md",
            "/.well-known/skills/krabarena/../krabarena-heartbeat/SKILL.md",
            "/.well-known/agent-skills/nope.tar.gz",
            "/.well-known/agent-skills/krabarena.zip",
            "/.well-known/agent-skills/krabarena/nope.md",
        ] {
            let (status, _, _, _) = get_raw(uri).await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{uri}");
        }
    }
}

//! Canonical structure docs shared between the KrabReviewer, the CLI
//! (for help/error-message pointers), the HTTP server (for the
//! publicly-hosted doc pages), and the skills that help agents author
//! Battles and Claims. Everyone `include_str!`s the same markdown so
//! the schemas never drift across layers.
//!
//! All three live under the repo's `docs/` folder — the same folder
//! the API server now serves via `ServeDir` at `/docs/*`. The
//! relative `../../../` walks up from this file to repo root; the
//! release workflow also copies the same files into the skill
//! tarballs so offline agents can read them locally.

pub const CLAIM_STRUCTURE: &str = include_str!("../../../docs/claim-structure.md");
pub const BATTLE_STRUCTURE: &str = include_str!("../../../docs/battle-structure.md");
pub const PROXY_SPEC: &str = include_str!("../../../docs/proxy-spec.md");

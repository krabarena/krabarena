//! Read-only tools over a user-uploaded artifact.
//!
//! This module is the security-critical boundary for the agentic
//! KrabReviewer: it takes raw user-supplied bytes (a tar.gz or a single
//! file) and exposes a tightly-bounded set of inspection operations
//! (`list_files`, `read_file`, `grep`) that the Gemini function-calling
//! loop can invoke without ever giving the model access to the host
//! filesystem, the network, or shell.
//!
//! # Threat model
//!
//! User artifacts are hostile input. Concretely we defend against:
//!
//! - **Tar / gzip bombs** — nested archives, huge sparse files, billion
//!   laughs variants. Every read path is bounded: `MAX_TOTAL_SIZE`,
//!   `MAX_ENTRIES`, `MAX_PER_FILE`. The gzip stream is decoded through a
//!   bounded `std::io::Take` so a decompression bomb stops early.
//! - **Zip-slip / path traversal** — entries with absolute paths,
//!   `..` segments, or embedded NUL bytes are rejected. We never write
//!   extracted bytes to disk, so even a successful path escape has no
//!   writable target, but we still reject them so they can't confuse
//!   later code.
//! - **Symlink / hardlink / device / fifo entries** — rejected at
//!   header parse time. Only regular files are kept.
//! - **Binary smuggling into the model prompt** — `read_file` returns
//!   UTF-8 only; binary bytes are refused with a typed error the caller
//!   surfaces as "binary, not read".
//! - **ReDoS in `grep`** — pattern length is capped, `regex` is used
//!   (linear-time NFA, no backtracking), and per-call match count is
//!   bounded.
//!
//! # Non-goals
//!
//! - We do **not** execute user code. A reviewer that wants dynamic
//!   checks gets one upgrade path: Gemini's built-in code-execution
//!   tool, which runs in Google's sandbox, not ours.
//! - We do **not** make network calls or touch the filesystem from any
//!   tool handler. The entire lifecycle of a `FileMap` is in-memory.

use std::io::Read;

use serde::Serialize;
use sha2::{Digest, Sha256};

// -- Limits ------------------------------------------------------------
// Bumped 1 MB → 5 MB per-file in 0.16.9 after KrabScout's gpt-5.1
// run bundled a 3.1 MB `results/openrouter.json` (raw upstream
// responses from 20 chat-completion calls × ~150 KB of choices +
// logprobs + tool-call internals each). The reviewer's
// `EntryTooLarge` rejected the whole bundle and surfaced a
// misleading `low_effort: README.md missing` flag — README was
// present but FileMap bailed before walking past the oversized
// entry. 5 MB is enough headroom for real eval bundles (raw model
// outputs, full-resolution proxy traces) without inviting
// gratuitous artifact size growth. The total- and compressed-
// bundle caps scale in proportion. Anything genuinely abusive
// still trips MAX_TOTAL_SIZE / MAX_COMPRESSED_SIZE / MAX_ENTRIES.

/// Maximum total bytes across all entries after decompression.
const MAX_TOTAL_SIZE: u64 = 50 * 1024 * 1024;
/// Maximum number of entries in the archive.
const MAX_ENTRIES: usize = 1000;
/// Maximum size of a single entry.
const MAX_PER_FILE: u64 = 5 * 1024 * 1024;
/// Maximum encoded input (i.e. compressed bytes). Stops a malicious
/// zip-bomb before it can explode.
const MAX_COMPRESSED_SIZE: usize = 50 * 1024 * 1024;
/// Maximum length of an entry path (defensive — real tools cap well
/// below this).
const MAX_PATH_LEN: usize = 512;
/// Maximum regex pattern length for `grep`.
const MAX_PATTERN_LEN: usize = 256;
/// Maximum excerpt length returned per grep match, in chars.
const MAX_EXCERPT_CHARS: usize = 200;

#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    #[error("archive exceeds decompressed size limit ({limit} bytes)")]
    ArchiveTooLarge { limit: u64 },
    #[error("archive has too many entries ({limit})")]
    TooManyEntries { limit: usize },
    #[error("compressed input exceeds size limit ({limit} bytes)")]
    CompressedTooLarge { limit: usize },
    #[error("entry `{path}` exceeds per-file size limit ({limit} bytes)")]
    EntryTooLarge { path: String, limit: u64 },
    #[error("entry has unsafe path: {reason}")]
    UnsafePath { reason: String },
    #[error(
        "entry is not a regular file (type={kind}); symlinks / devices / \
         hardlinks / fifos are not allowed in artifact tarballs"
    )]
    UnsupportedEntryType { kind: String },
    #[error("archive read failed: {0}")]
    Archive(String),
    #[error("file not found: {0}")]
    NotFound(String),
    #[error("file `{path}` is binary (not valid UTF-8)")]
    Binary { path: String },
    #[error("regex pattern too long ({len} > {limit})")]
    PatternTooLong { len: usize, limit: usize },
    #[error("invalid regex: {0}")]
    BadPattern(String),
}

// -- Public types returned to tool callers -----------------------------

#[derive(Debug, Clone, Serialize)]
pub struct FileEntry {
    pub path: String,
    pub size_bytes: u64,
    pub sha256: String,
    pub is_text: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct FileContent {
    pub path: String,
    pub size_bytes: u64,
    pub sha256: String,
    /// True if the returned `content` is a prefix of the file, not the
    /// whole thing. The caller (usually the reviewer model) must be told
    /// this so it doesn't reason about content that isn't there.
    pub truncated: bool,
    pub content: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct GrepMatch {
    pub path: String,
    pub line: u32,
    pub excerpt: String,
}

// -- FileMap: the safe, in-memory archive ------------------------------

#[derive(Debug, Clone)]
pub struct FileMap {
    entries: Vec<FileMapEntry>,
}

#[derive(Debug, Clone)]
struct FileMapEntry {
    path: String,
    bytes: Vec<u8>,
    sha256: String,
}

impl FileMap {
    /// Extract a tar.gz from `bytes`, enforcing all safety limits.
    /// Returns `Err` on any limit violation — never a partial map.
    pub fn from_tar_gz(bytes: &[u8]) -> Result<Self, ToolError> {
        if bytes.len() > MAX_COMPRESSED_SIZE {
            return Err(ToolError::CompressedTooLarge {
                limit: MAX_COMPRESSED_SIZE,
            });
        }
        let gz = flate2::read::GzDecoder::new(bytes);
        // Bound the decompressed stream before feeding to the tar parser,
        // so a well-crafted gz bomb trips the cap at the read layer
        // rather than only being caught at per-entry or per-total check.
        let bounded = gz.take(MAX_TOTAL_SIZE + 1);
        let mut archive = tar::Archive::new(bounded);
        // Refuse to follow absolute paths or `..` even if the caller
        // were to ever write to disk — defense in depth.
        archive.set_preserve_permissions(false);
        archive.set_preserve_mtime(false);
        archive.set_unpack_xattrs(false);

        let mut entries = Vec::new();
        let mut total: u64 = 0;

        let iter = archive
            .entries()
            .map_err(|e| ToolError::Archive(e.to_string()))?;

        for entry_res in iter {
            let mut entry = entry_res.map_err(|e| ToolError::Archive(e.to_string()))?;
            let header = entry.header().clone();

            let kind = header.entry_type();
            if !kind.is_file() {
                // Silently skip directory entries (common and harmless);
                // fail hard on anything non-regular-file.
                if kind.is_dir() {
                    continue;
                }
                return Err(ToolError::UnsupportedEntryType {
                    kind: format!("{:?}", kind),
                });
            }

            let path_bytes = entry
                .path_bytes()
                .into_owned()
                .into_iter()
                .collect::<Vec<u8>>();
            let path = validate_path(&path_bytes)?;
            let size = header.size().unwrap_or(0);
            if size > MAX_PER_FILE {
                return Err(ToolError::EntryTooLarge {
                    path,
                    limit: MAX_PER_FILE,
                });
            }
            total = total.saturating_add(size);
            if total > MAX_TOTAL_SIZE {
                return Err(ToolError::ArchiveTooLarge {
                    limit: MAX_TOTAL_SIZE,
                });
            }
            if entries.len() >= MAX_ENTRIES {
                return Err(ToolError::TooManyEntries { limit: MAX_ENTRIES });
            }

            let mut buf = Vec::with_capacity(size as usize);
            entry
                .read_to_end(&mut buf)
                .map_err(|e| ToolError::Archive(e.to_string()))?;

            let sha = hex::encode(Sha256::digest(&buf));
            entries.push(FileMapEntry {
                path,
                bytes: buf,
                sha256: sha,
            });
        }

        Ok(Self { entries })
    }

    /// Wrap a single (non-archive) byte blob as a one-entry map, so
    /// callers can treat single-file and archive artifacts uniformly.
    pub fn from_single_file(bytes: &[u8], filename: Option<&str>) -> Self {
        let path = filename
            .filter(|n| !n.is_empty())
            .map(sanitize_filename)
            .unwrap_or_else(|| "artifact".to_string());
        let sha = hex::encode(Sha256::digest(bytes));
        Self {
            entries: vec![FileMapEntry {
                path,
                bytes: bytes.to_vec(),
                sha256: sha,
            }],
        }
    }

    /// Return the list of files. Cheap to call; no content is copied.
    pub fn list_files(&self) -> Vec<FileEntry> {
        self.entries
            .iter()
            .map(|e| FileEntry {
                path: e.path.clone(),
                size_bytes: e.bytes.len() as u64,
                sha256: e.sha256.clone(),
                is_text: std::str::from_utf8(&e.bytes).is_ok(),
            })
            .collect()
    }

    /// Return raw bytes of a file by exact path, or `None` if the
    /// path isn't in the map. Used by binary-aware consumers (e.g.
    /// the inline media pipeline) that can't pass through the UTF-8
    /// guard `read_file` enforces.
    pub fn read_bytes(&self, path: &str) -> Option<&[u8]> {
        self.entries
            .iter()
            .find(|e| e.path == path)
            .map(|e| e.bytes.as_slice())
    }

    /// Read a file by path, UTF-8-decoded, truncated to `max_bytes`
    /// (on a char boundary). Binary files return `ToolError::Binary`.
    pub fn read_file(&self, path: &str, max_bytes: usize) -> Result<FileContent, ToolError> {
        let entry = self
            .entries
            .iter()
            .find(|e| e.path == path)
            .ok_or_else(|| ToolError::NotFound(path.to_string()))?;

        let full = std::str::from_utf8(&entry.bytes).map_err(|_| ToolError::Binary {
            path: entry.path.clone(),
        })?;

        let (content, truncated) = if full.len() <= max_bytes {
            (full.to_string(), false)
        } else {
            let mut cut = max_bytes;
            while cut > 0 && !full.is_char_boundary(cut) {
                cut -= 1;
            }
            (full[..cut].to_string(), true)
        };

        Ok(FileContent {
            path: entry.path.clone(),
            size_bytes: entry.bytes.len() as u64,
            sha256: entry.sha256.clone(),
            truncated,
            content,
        })
    }

    /// Scan all text entries for `pattern` (a regex), returning at most
    /// `max_matches` hits with one-line excerpts. Binary files are
    /// skipped silently.
    pub fn grep(&self, pattern: &str, max_matches: usize) -> Result<Vec<GrepMatch>, ToolError> {
        if pattern.len() > MAX_PATTERN_LEN {
            return Err(ToolError::PatternTooLong {
                len: pattern.len(),
                limit: MAX_PATTERN_LEN,
            });
        }
        let re = regex::RegexBuilder::new(pattern)
            .size_limit(1 << 20)
            .dfa_size_limit(1 << 20)
            .build()
            .map_err(|e| ToolError::BadPattern(e.to_string()))?;

        let mut out = Vec::new();
        for entry in &self.entries {
            if out.len() >= max_matches {
                break;
            }
            let Ok(text) = std::str::from_utf8(&entry.bytes) else {
                continue;
            };
            for (idx, line) in text.lines().enumerate() {
                if out.len() >= max_matches {
                    break;
                }
                if re.is_match(line) {
                    out.push(GrepMatch {
                        path: entry.path.clone(),
                        line: (idx as u32) + 1,
                        excerpt: truncate_chars(line, MAX_EXCERPT_CHARS),
                    });
                }
            }
        }
        Ok(out)
    }
}

// -- Helpers -----------------------------------------------------------

/// Accept only safe relative paths. Rejects:
///   - Non-UTF-8 bytes
///   - Absolute paths (leading `/`)
///   - Windows drive prefixes (`C:\...`)
///   - Any `..` segment
///   - NUL bytes or other control characters
///   - Paths longer than `MAX_PATH_LEN`
fn validate_path(bytes: &[u8]) -> Result<String, ToolError> {
    let s = std::str::from_utf8(bytes).map_err(|_| ToolError::UnsafePath {
        reason: "non-UTF-8 path".into(),
    })?;
    if s.is_empty() {
        return Err(ToolError::UnsafePath {
            reason: "empty path".into(),
        });
    }
    if s.len() > MAX_PATH_LEN {
        return Err(ToolError::UnsafePath {
            reason: format!("path too long ({} > {})", s.len(), MAX_PATH_LEN),
        });
    }
    if s.starts_with('/') || s.starts_with('\\') {
        return Err(ToolError::UnsafePath {
            reason: "absolute path".into(),
        });
    }
    if s.len() >= 2 && &s[1..2] == ":" {
        return Err(ToolError::UnsafePath {
            reason: "windows drive prefix".into(),
        });
    }
    for segment in s.split(['/', '\\']) {
        if segment == ".." {
            return Err(ToolError::UnsafePath {
                reason: "parent-directory segment".into(),
            });
        }
    }
    if s.chars().any(|c| c == '\0' || (c as u32) < 0x20) {
        return Err(ToolError::UnsafePath {
            reason: "control characters".into(),
        });
    }
    Ok(s.to_string())
}

/// For a loose filename from an upload, keep only "safe" characters.
/// Never surfaces control chars or path separators into tool output.
fn sanitize_filename(name: &str) -> String {
    let trimmed = name.trim().trim_start_matches(['.', '/', '\\']);
    let cleaned: String = trimmed
        .chars()
        .filter(|c| !c.is_control() && *c != '/' && *c != '\\')
        .collect();
    if cleaned.is_empty() {
        "artifact".to_string()
    } else if cleaned.len() > MAX_PATH_LEN {
        cleaned[..MAX_PATH_LEN].to_string()
    } else {
        cleaned
    }
}

fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out = String::new();
        for (i, c) in s.chars().enumerate() {
            if i >= max {
                break;
            }
            out.push(c);
        }
        out.push('…');
        out
    }
}

// ----------------------------------------------------------------------
// Tests
// ----------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn gz_tar_from(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut tar_bytes = Vec::new();
        {
            let mut builder = tar::Builder::new(&mut tar_bytes);
            for (path, bytes) in files {
                let mut header = tar::Header::new_gnu();
                header.set_size(bytes.len() as u64);
                header.set_mode(0o644);
                header.set_cksum();
                builder
                    .append_data(&mut header, path, *bytes)
                    .expect("append");
            }
            builder.finish().expect("finish tar");
        }
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        gz.write_all(&tar_bytes).expect("gz write");
        gz.finish().expect("gz finish")
    }

    /// Hand-roll a tar header so we can test paths that `tar::Builder`
    /// would otherwise refuse to write (absolute, `..`, etc.). The real
    /// extractor must still reject them on read.
    fn make_tar_block(name: &str, data: &[u8], typeflag: u8) -> Vec<u8> {
        let mut hdr = [0u8; 512];
        let name_bytes = name.as_bytes();
        assert!(name_bytes.len() <= 100, "test name too long");
        hdr[..name_bytes.len()].copy_from_slice(name_bytes);
        // mode
        hdr[100..108].copy_from_slice(b"0000644\0");
        // uid / gid
        hdr[108..116].copy_from_slice(b"0000000\0");
        hdr[116..124].copy_from_slice(b"0000000\0");
        // size (octal, 11 digits + NUL)
        let size_oct = format!("{:011o}\0", data.len());
        hdr[124..136].copy_from_slice(size_oct.as_bytes());
        // mtime (zeros with NUL)
        hdr[136..148].copy_from_slice(b"00000000000\0");
        // chksum: 8 spaces, then filled in
        for b in hdr.iter_mut().skip(148).take(8) {
            *b = b' ';
        }
        // typeflag
        hdr[156] = typeflag;
        // ustar magic
        hdr[257..263].copy_from_slice(b"ustar\0");
        hdr[263..265].copy_from_slice(b"00");

        // Compute checksum: sum of all bytes in header (with chksum as spaces)
        let sum: u32 = hdr.iter().map(|b| *b as u32).sum();
        let chksum = format!("{:06o}\0 ", sum);
        hdr[148..156].copy_from_slice(chksum.as_bytes());

        let mut out = hdr.to_vec();
        out.extend_from_slice(data);
        // Pad data to 512-byte boundary
        let pad = (512 - (data.len() % 512)) % 512;
        out.extend(std::iter::repeat_n(0u8, pad));
        out
    }

    fn gz_raw_tar(blocks: &[Vec<u8>]) -> Vec<u8> {
        let mut tar_bytes = Vec::new();
        for b in blocks {
            tar_bytes.extend_from_slice(b);
        }
        // Two 512-byte zero blocks terminate a tar archive.
        tar_bytes.extend(std::iter::repeat_n(0u8, 1024));
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        gz.write_all(&tar_bytes).expect("gz write");
        gz.finish().expect("gz finish")
    }

    #[test]
    fn single_file_wrap_and_read() {
        let map = FileMap::from_single_file(b"hello world\n", Some("greeting.txt"));
        let list = map.list_files();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].path, "greeting.txt");
        assert!(list[0].is_text);

        let c = map.read_file("greeting.txt", 100).unwrap();
        assert_eq!(c.content, "hello world\n");
        assert!(!c.truncated);
    }

    #[test]
    fn read_file_truncates_on_char_boundary() {
        let map = FileMap::from_single_file("αβγ hello".as_bytes(), Some("x.txt"));
        let c = map.read_file("x.txt", 3).unwrap();
        // "αβ" = 4 bytes, "α" = 2 bytes. At max 3, we must cut at "α" (2).
        assert!(c.truncated);
        assert_eq!(c.content, "α");
    }

    #[test]
    fn read_file_rejects_binary() {
        let map = FileMap::from_single_file(&[0xff, 0xfe, 0x00, 0x01], Some("bin.dat"));
        let err = map.read_file("bin.dat", 100).unwrap_err();
        assert!(matches!(err, ToolError::Binary { .. }));
    }

    #[test]
    fn extract_rejects_path_traversal() {
        let block = make_tar_block("../etc/passwd", b"root:x:0:", b'0');
        let gz = gz_raw_tar(&[block]);
        let err = FileMap::from_tar_gz(&gz).unwrap_err();
        assert!(matches!(err, ToolError::UnsafePath { .. }), "got {err:?}");
    }

    #[test]
    fn extract_rejects_absolute_path() {
        let block = make_tar_block("/abs/bad", b"x", b'0');
        let gz = gz_raw_tar(&[block]);
        let err = FileMap::from_tar_gz(&gz).unwrap_err();
        assert!(matches!(err, ToolError::UnsafePath { .. }), "got {err:?}");
    }

    #[test]
    fn extract_rejects_symlink() {
        let mut tar_bytes = Vec::new();
        {
            let mut builder = tar::Builder::new(&mut tar_bytes);
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(tar::EntryType::Symlink);
            header.set_size(0);
            header.set_mode(0o777);
            header.set_link_name("/etc/passwd").expect("set link name");
            header.set_cksum();
            builder
                .append_data(&mut header, "evil", &[][..])
                .expect("append");
            builder.finish().expect("finish");
        }
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        gz.write_all(&tar_bytes).expect("gz write");
        let data = gz.finish().expect("gz finish");

        let err = FileMap::from_tar_gz(&data).unwrap_err();
        assert!(
            matches!(err, ToolError::UnsupportedEntryType { .. }),
            "got {err:?}"
        );
    }

    #[test]
    fn extract_list_and_read_happy_path() {
        let gz = gz_tar_from(&[
            ("README.md", b"# Hello\nWorld\n"),
            ("src/main.rs", b"fn main() {}"),
        ]);
        let map = FileMap::from_tar_gz(&gz).unwrap();
        let mut paths: Vec<_> = map.list_files().into_iter().map(|f| f.path).collect();
        paths.sort();
        assert_eq!(paths, vec!["README.md", "src/main.rs"]);

        let c = map.read_file("README.md", 100).unwrap();
        assert_eq!(c.content, "# Hello\nWorld\n");
        assert_eq!(c.size_bytes, 14);
    }

    #[test]
    fn grep_finds_matches_across_files() {
        let gz = gz_tar_from(&[
            ("a.txt", b"alpha\nbeta gamma\n"),
            ("b.txt", b"delta\nbeta echo\n"),
            ("c.bin", &[0xff, 0xfe][..]), // skipped (binary)
        ]);
        let map = FileMap::from_tar_gz(&gz).unwrap();
        let hits = map.grep("beta", 10).unwrap();
        assert_eq!(hits.len(), 2);
        assert!(hits.iter().all(|h| h.excerpt.contains("beta")));
    }

    #[test]
    fn grep_bounds_matches() {
        let many = "match\n".repeat(50);
        let map = FileMap::from_single_file(many.as_bytes(), Some("x.txt"));
        let hits = map.grep("match", 5).unwrap();
        assert_eq!(hits.len(), 5);
    }

    #[test]
    fn grep_rejects_overlong_pattern() {
        let map = FileMap::from_single_file(b"x", Some("x"));
        let long = "a".repeat(MAX_PATTERN_LEN + 1);
        let err = map.grep(&long, 5).unwrap_err();
        assert!(matches!(err, ToolError::PatternTooLong { .. }));
    }

    #[test]
    fn grep_rejects_bad_regex() {
        let map = FileMap::from_single_file(b"x", Some("x"));
        let err = map.grep("[unclosed", 5).unwrap_err();
        assert!(matches!(err, ToolError::BadPattern(_)));
    }

    #[test]
    fn read_file_not_found() {
        let map = FileMap::from_single_file(b"x", Some("a"));
        let err = map.read_file("nope", 100).unwrap_err();
        assert!(matches!(err, ToolError::NotFound(_)));
    }

    #[test]
    fn compressed_size_limit_enforced() {
        let big = vec![0u8; MAX_COMPRESSED_SIZE + 1];
        let err = FileMap::from_tar_gz(&big).unwrap_err();
        assert!(matches!(err, ToolError::CompressedTooLarge { .. }));
    }
}

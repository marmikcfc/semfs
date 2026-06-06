//! Virtual read-only `profile.md` backed by the `POST /v4/profile` API.

use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::RwLock;

use crate::api::{ApiClient, ProfileResp};
use crate::vfs::error::{VfsError, VfsResult};
use crate::vfs::mode::S_IFREG;
use crate::vfs::types::{FileAttr, Timestamp};

pub const PROFILE_INO: u64 = u64::MAX - 1;
pub const PROFILE_NAME: &str = "profile.md";

#[derive(Debug)]
pub struct ProfileFile {
    api: Arc<ApiClient>,
    cache: RwLock<Option<Vec<u8>>>,
}

impl ProfileFile {
    pub fn new(api: Arc<ApiClient>) -> Self {
        Self {
            api,
            cache: RwLock::new(None),
        }
    }

    pub async fn warm(&self) {
        match self.api.get_profile().await {
            Ok(resp) => {
                *self.cache.write() = Some(format_profile(&resp).into_bytes());
            }
            Err(e) => {
                tracing::warn!(error = %e, "profile warm failed; profile.md will be empty until next mount");
            }
        }
    }

    /// True if the warmed profile carries real content (cloud memories). When
    /// false (local-only mounts, or a container with no extracted memories), the
    /// caller can substitute a locally-generated overview.
    pub fn is_substantive(&self) -> bool {
        self.cache
            .read()
            .as_ref()
            .map(|v| v.len() > 220) // header alone is ~158B; a memory adds more
            .unwrap_or(false)
    }

    /// Overwrite the profile content (used to inject a locally-generated overview
    /// when the cloud profile is empty).
    pub fn set_content(&self, bytes: Vec<u8>) {
        *self.cache.write() = Some(bytes);
    }

    pub fn profile_attr(&self) -> FileAttr {
        let now = Timestamp::now();
        let size = self
            .cache
            .read()
            .as_ref()
            .map(|v| v.len() as u64)
            .unwrap_or(0);
        FileAttr {
            ino: PROFILE_INO,
            mode: S_IFREG | 0o444,
            nlink: 1,
            uid: 0,
            gid: 0,
            size,
            blocks: size.div_ceil(512),
            atime: now,
            mtime: now,
            ctime: now,
            rdev: 0,
            blksize: 4096,
        }
    }
}

#[async_trait]
impl crate::vfs::traits::File for ProfileFile {
    async fn read(&self, offset: u64, size: usize) -> VfsResult<Vec<u8>> {
        let cache = self.cache.read();
        let Some(content) = cache.as_ref() else {
            return Ok(Vec::new());
        };
        let offset = offset as usize;
        if offset >= content.len() {
            return Ok(Vec::new());
        }
        let end = (offset + size).min(content.len());
        Ok(content[offset..end].to_vec())
    }

    async fn write(&self, _offset: u64, _data: &[u8]) -> VfsResult<u32> {
        Err(VfsError::PermissionDenied)
    }

    async fn truncate(&self, _size: u64) -> VfsResult<()> {
        Err(VfsError::PermissionDenied)
    }

    async fn flush(&self) -> VfsResult<()> {
        Ok(())
    }

    async fn fsync(&self) -> VfsResult<()> {
        Ok(())
    }

    async fn getattr(&self) -> VfsResult<FileAttr> {
        Ok(self.profile_attr())
    }
}

/// Build a local container overview from the indexed file paths — a stand-in for
/// the cloud `/v4/profile` memories on local-only mounts. Gives an agent the
/// directory map + a search-first instruction up front so it doesn't enumerate
/// the tree (`os.walk`) or cat files to discover structure.
pub fn build_local_profile(paths: &[String]) -> String {
    use std::collections::BTreeMap;
    let mut dir_counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut ext_counts: BTreeMap<String, usize> = BTreeMap::new();
    for p in paths {
        let trimmed = p.trim_start_matches('/');
        let comps: Vec<&str> = trimmed.split('/').collect();
        // Aggregate by top-1 and top-2 directory levels.
        if comps.len() >= 2 {
            *dir_counts.entry(format!("/{}", comps[0])).or_default() += 1;
        }
        if comps.len() >= 3 {
            *dir_counts
                .entry(format!("/{}/{}", comps[0], comps[1]))
                .or_default() += 1;
        }
        let ext = p.rsplit('.').next().filter(|e| !e.contains('/')).unwrap_or("?");
        if ext != p.as_str() {
            *ext_counts.entry(ext.to_lowercase()).or_default() += 1;
        }
    }
    let mut out = String::new();
    out.push_str("# Container Overview (local semantic index)\n\n");
    out.push_str(&format!(
        "This mount has {} indexed files. SEARCH FIRST: run `semfs grep \"<natural language query>\"` — it \
         returns ranked semantic excerpts across every file. The directory map and file types below are the \
         whole container; you do NOT need to walk the tree or cat files to discover structure.\n\n",
        paths.len()
    ));
    out.push_str("## Directory map (indexed file counts)\n");
    // Sort dirs by descending count, cap the list so profile.md stays compact.
    let mut dirs: Vec<(&String, &usize)> = dir_counts.iter().collect();
    dirs.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
    for (dir, n) in dirs.into_iter().take(40) {
        let depth = dir.matches('/').count();
        let indent = if depth > 1 { "  " } else { "" };
        out.push_str(&format!("{indent}{dir}/  ({n})\n"));
    }
    out.push_str("\n## File types\n");
    let mut exts: Vec<(&String, &usize)> = ext_counts.iter().collect();
    exts.sort_by(|a, b| b.1.cmp(a.1));
    let exts_str: Vec<String> = exts
        .into_iter()
        .take(15)
        .map(|(e, n)| format!("{e}({n})"))
        .collect();
    out.push_str(&exts_str.join(" "));
    out.push('\n');
    out
}

fn format_profile(resp: &ProfileResp) -> String {
    let mut out = String::new();
    out.push_str("# Memory Profile\n");
    out.push_str("# This file is auto-generated from your memories.\n");
    out.push_str("# It is not editable. To update, modify the source files\n");
    out.push_str("# that contain this information.\n\n");

    if let Some(statics) = &resp.profile.static_memories {
        if !statics.is_empty() {
            out.push_str("## Core Knowledge\n");
            for mem in statics {
                out.push_str(&format!("- {}\n", mem));
            }
            out.push('\n');
        }
    }

    if let Some(dynamics) = &resp.profile.dynamic {
        if !dynamics.is_empty() {
            out.push_str("## Recent Context\n");
            for mem in dynamics {
                out.push_str(&format!("- {}\n", mem));
            }
        }
    }

    if out.lines().count() <= 4 {
        out.push_str("(No memories yet. Write files to generate memories.)\n");
    }

    out
}

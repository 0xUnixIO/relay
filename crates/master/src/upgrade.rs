//! Remote upgrade: tag resolver + GitHub releases client.
//!
//! Inputs from HTTP layer come as `target = "stable" | "rc" | "vX.Y.Z(-rc.*)?"`.
//! This module resolves them to a concrete release with download URLs, with a
//! 1-hour in-memory cache to avoid GitHub's 60/h unauthenticated rate limit.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

const CACHE_TTL: Duration = Duration::from_secs(3600);
const HTTP_TIMEOUT: Duration = Duration::from_secs(10);
pub const DEFAULT_REPO: &str = "0xUnixIO/relay";

/// Validate a release tag against the project convention `vMAJOR.MINOR.PATCH`
/// optionally followed by `-rc.<alphanumeric.dotted>`. Examples:
///   v0.2.0, v0.2.0-rc.20260430232511
pub fn validate_tag(tag: &str) -> bool {
    let Some(rest) = tag.strip_prefix('v') else {
        return false;
    };
    let (ver, rc) = match rest.find("-rc.") {
        Some(i) => (&rest[..i], Some(&rest[i + 4..])),
        None => (rest, None),
    };
    let parts: Vec<&str> = ver.split('.').collect();
    if parts.len() != 3 {
        return false;
    }
    if !parts
        .iter()
        .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
    {
        return false;
    }
    if let Some(rc) = rc {
        if rc.is_empty() {
            return false;
        }
        if !rc.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'.') {
            return false;
        }
    }
    true
}

/// Strip leading `v` and any trailing build metadata (after `+`) so two tags
/// can be compared loosely. We only use this for "is heartbeat == target?"
/// comparisons, not for ordering.
pub fn normalize_version(s: &str) -> String {
    let s = s.strip_prefix('v').unwrap_or(s);
    match s.find('+') {
        Some(i) => s[..i].to_string(),
        None => s.to_string(),
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ResolvedRelease {
    pub tag: String,
    pub prerelease: bool,
    pub published_at: Option<String>,
    /// URL of `relay-node-${tag}-x86_64-unknown-linux-gnu.tar.gz`.
    pub linux_amd64_url: Option<String>,
    /// URL of `relay-node-${tag}-aarch64-unknown-linux-gnu.tar.gz`.
    pub linux_arm64_url: Option<String>,
    /// URL of `relay-master-${tag}-x86_64-unknown-linux-gnu.tar.gz`.
    pub master_linux_amd64_url: Option<String>,
    /// URL of `relay-master-${tag}-aarch64-unknown-linux-gnu.tar.gz`.
    pub master_linux_arm64_url: Option<String>,
    pub sha256_url: Option<String>,
}

#[derive(Deserialize)]
struct GhRelease {
    tag_name: String,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    published_at: Option<String>,
    #[serde(default)]
    assets: Vec<GhAsset>,
}

#[derive(Deserialize)]
struct GhAsset {
    name: String,
    browser_download_url: String,
}

impl From<GhRelease> for ResolvedRelease {
    fn from(r: GhRelease) -> Self {
        let mut node_amd64 = None;
        let mut node_arm64 = None;
        let mut master_amd64 = None;
        let mut master_arm64 = None;
        let mut sums = None;
        for a in r.assets {
            if a.name == "SHA256SUMS" {
                sums = Some(a.browser_download_url);
            } else if a.name.starts_with("relay-node-")
                && a.name.ends_with("x86_64-unknown-linux-gnu.tar.gz")
            {
                node_amd64 = Some(a.browser_download_url);
            } else if a.name.starts_with("relay-node-")
                && a.name.ends_with("aarch64-unknown-linux-gnu.tar.gz")
            {
                node_arm64 = Some(a.browser_download_url);
            } else if a.name.starts_with("relay-master-")
                && a.name.ends_with("x86_64-unknown-linux-gnu.tar.gz")
            {
                master_amd64 = Some(a.browser_download_url);
            } else if a.name.starts_with("relay-master-")
                && a.name.ends_with("aarch64-unknown-linux-gnu.tar.gz")
            {
                master_arm64 = Some(a.browser_download_url);
            }
        }
        ResolvedRelease {
            tag: r.tag_name,
            prerelease: r.prerelease,
            published_at: r.published_at,
            linux_amd64_url: node_amd64,
            linux_arm64_url: node_arm64,
            master_linux_amd64_url: master_amd64,
            master_linux_arm64_url: master_arm64,
            sha256_url: sums,
        }
    }
}

#[derive(Default)]
struct CacheInner {
    stable: Option<(Instant, ResolvedRelease)>,
    by_tag: HashMap<String, (Instant, ResolvedRelease)>,
}

#[derive(Clone)]
pub struct UpgradeResolver {
    client: reqwest::Client,
    cache: Arc<Mutex<CacheInner>>,
    repo: String,
}

impl UpgradeResolver {
    pub fn new(repo: impl Into<String>) -> Self {
        let client = reqwest::Client::builder()
            .timeout(HTTP_TIMEOUT)
            .user_agent(concat!("relay-master/", env!("CARGO_PKG_VERSION")))
            .build()
            .expect("reqwest client build");
        Self {
            client,
            cache: Arc::new(Mutex::new(CacheInner::default())),
            repo: repo.into(),
        }
    }

    /// Resolve a target spec to a concrete release. Accepts `"stable"` or an explicit `vX.Y.Z` tag.
    pub async fn resolve(&self, target: &str) -> anyhow::Result<ResolvedRelease> {
        match target {
            "stable" => self.latest_stable().await,
            t if validate_tag(t) => self.lookup_tag(t).await,
            t => anyhow::bail!("invalid upgrade target: {t}"),
        }
    }

    pub async fn latest_stable(&self) -> anyhow::Result<ResolvedRelease> {
        {
            let cache = self.cache.lock().await;
            if let Some((at, rel)) = &cache.stable {
                if at.elapsed() < CACHE_TTL {
                    return Ok(rel.clone());
                }
            }
        }

        let url = format!(
            "https://api.github.com/repos/{}/releases?per_page=10",
            self.repo
        );
        let resp = self
            .client
            .get(&url)
            .header("Accept", "application/vnd.github+json")
            .send()
            .await?;
        if !resp.status().is_success() {
            anyhow::bail!("github releases list returned {}", resp.status());
        }
        let releases: Vec<GhRelease> = resp.json().await?;
        let release = releases
            .into_iter()
            .find(|r| !r.prerelease)
            .ok_or_else(|| anyhow::anyhow!("no stable release found"))?;
        let resolved: ResolvedRelease = release.into();

        let mut cache = self.cache.lock().await;
        cache.stable = Some((Instant::now(), resolved.clone()));
        cache
            .by_tag
            .insert(resolved.tag.clone(), (Instant::now(), resolved.clone()));
        Ok(resolved)
    }

    async fn lookup_tag(&self, tag: &str) -> anyhow::Result<ResolvedRelease> {
        {
            let cache = self.cache.lock().await;
            if let Some((at, rel)) = cache.by_tag.get(tag) {
                if at.elapsed() < CACHE_TTL {
                    return Ok(rel.clone());
                }
            }
        }
        let url = format!(
            "https://api.github.com/repos/{}/releases/tags/{}",
            self.repo, tag
        );
        let resp = self
            .client
            .get(&url)
            .header("Accept", "application/vnd.github+json")
            .send()
            .await?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            anyhow::bail!("release tag {tag} not found");
        }
        if !resp.status().is_success() {
            anyhow::bail!("github release fetch returned {}", resp.status());
        }
        let r: GhRelease = resp.json().await?;
        let resolved: ResolvedRelease = r.into();
        {
            let mut cache = self.cache.lock().await;
            cache
                .by_tag
                .insert(resolved.tag.clone(), (Instant::now(), resolved.clone()));
        }
        Ok(resolved)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_tag_accepts_release() {
        assert!(validate_tag("v0.2.0"));
        assert!(validate_tag("v10.20.30"));
    }

    #[test]
    fn validate_tag_accepts_rc() {
        assert!(validate_tag("v0.2.0-rc.20260430232511"));
        assert!(validate_tag("v1.0.0-rc.1"));
        assert!(validate_tag("v1.0.0-rc.alpha.2"));
    }

    #[test]
    fn validate_tag_rejects_garbage() {
        assert!(!validate_tag(""));
        assert!(!validate_tag("0.2.0"));
        assert!(!validate_tag("v0.2"));
        assert!(!validate_tag("v0.2.0.0"));
        assert!(!validate_tag("v0.2.0-beta.1"));
        assert!(!validate_tag("v0.2.0-rc."));
        assert!(!validate_tag("v0.2.0-rc.@@"));
        assert!(!validate_tag("v0.a.0"));
        assert!(!validate_tag("vv0.2.0"));
    }

    #[test]
    fn normalize_strips_v_and_metadata() {
        assert_eq!(normalize_version("v0.2.0"), "0.2.0");
        assert_eq!(normalize_version("0.2.0"), "0.2.0");
        assert_eq!(normalize_version("v0.2.0+gabcdef"), "0.2.0");
    }
}

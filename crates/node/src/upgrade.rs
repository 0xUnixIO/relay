//! Node-side upgrade command handler.
//!
//! Receives `Command{kind=UPGRADE_AGENT}` from master, validates the args,
//! then downloads the new binary in the background, atomically replaces the
//! current binary, and calls process::exit(0) so systemd restarts the service.

use relay_proto::v1::{node_message::Payload as NodePayload, Command, NodeMessage, UpgradeReport};
use std::sync::OnceLock;
use tokio::sync::{mpsc, Mutex};

/// Serialize concurrent UPGRADE_AGENT commands — only one upgrade runs at a time.
fn upgrade_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

pub async fn handle_upgrade_command(cmd: Command, tx: mpsc::Sender<NodeMessage>) {
    let job_id_s = cmd.args.get("job_id").cloned().unwrap_or_default();
    let tag = cmd.args.get("tag").cloned().unwrap_or_default();
    let amd64_url = cmd.args.get("asset_url_amd64").cloned().unwrap_or_default();
    let arm64_url = cmd.args.get("asset_url_arm64").cloned().unwrap_or_default();
    let sha256_url = cmd.args.get("sha256_url").cloned().unwrap_or_default();

    let job_id: i64 = match job_id_s.parse() {
        Ok(n) => n,
        Err(_) => {
            tracing::warn!(job_id_s, "upgrade: invalid job_id");
            return;
        }
    };

    if let Err(reason) = validate(&tag, &amd64_url, &arm64_url, &sha256_url) {
        tracing::warn!(%tag, %reason, "upgrade: rejected");
        send_report(&tx, job_id, "failed", &reason).await;
        return;
    }

    let _guard = upgrade_lock().try_lock();
    if _guard.is_err() {
        let reason = "another upgrade is already in progress";
        tracing::warn!("upgrade: refused, {reason}");
        send_report(&tx, job_id, "failed", reason).await;
        return;
    }

    let asset_url = match std::env::consts::ARCH {
        "x86_64" => amd64_url,
        "aarch64" => arm64_url,
        a => {
            let reason = format!("unsupported arch: {a}");
            send_report(&tx, job_id, "failed", &reason).await;
            return;
        }
    };

    tracing::info!(job_id, %tag, "upgrade: accepted, downloading in background");
    send_report(&tx, job_id, "accepted", "").await;

    // 后台执行：下载 → 验证 → 原子替换 → exit(0)
    tokio::spawn(async move {
        match do_node_upgrade(&asset_url, &sha256_url, &tag).await {
            Ok(()) => {
                tracing::info!(job_id, %tag, "upgrade: binary replaced, exiting for systemd restart");
                std::process::exit(0);
            }
            Err(e) => {
                tracing::error!(job_id, error = %e, "upgrade failed");
            }
        }
    });
}

async fn do_node_upgrade(asset_url: &str, sha_url: &str, tag: &str) -> anyhow::Result<()> {
    use anyhow::Context;
    use flate2::read::GzDecoder;
    use sha2::{Digest, Sha256};
    use std::io::Read as _;
    use std::time::Duration;

    const DEST: &str = "/var/lib/relay-node/relay-node";

    let basename = asset_url
        .rsplit('/')
        .next()
        .with_context(|| format!("bad asset URL: {asset_url}"))?;

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(300))
        .user_agent(concat!("relay-node/", env!("CARGO_PKG_VERSION")))
        .build()?;

    tracing::info!(%asset_url, "upgrade: downloading tarball");
    let tar_data = client
        .get(asset_url)
        .send()
        .await
        .context("download tarball")?
        .error_for_status()
        .context("tarball response")?
        .bytes()
        .await
        .context("read tarball bytes")?;

    tracing::info!(%sha_url, "upgrade: downloading SHA256SUMS");
    let sums_data = reqwest::Client::builder()
        .timeout(Duration::from_secs(60))
        .build()?
        .get(sha_url)
        .send()
        .await
        .context("download SHA256SUMS")?
        .error_for_status()
        .context("SHA256SUMS response")?
        .bytes()
        .await
        .context("read SHA256SUMS bytes")?;

    let sums_str = std::str::from_utf8(&sums_data).context("SHA256SUMS not UTF-8")?;
    let expected = sums_str
        .lines()
        .filter_map(|l| {
            let l = l.trim();
            let (hash, fname) = l.split_once("  ").or_else(|| l.split_once(' '))?;
            (fname.trim() == basename).then(|| hash.trim().to_string())
        })
        .next()
        .with_context(|| format!("{basename} not found in SHA256SUMS"))?;
    let actual = hex::encode(Sha256::digest(&tar_data));
    anyhow::ensure!(
        actual == expected,
        "checksum mismatch: expected {expected}, got {actual}"
    );

    tracing::info!(%tag, "upgrade: extracting relay-node from archive");
    let mut archive = tar::Archive::new(GzDecoder::new(tar_data.as_ref()));
    let mut bin_data: Option<Vec<u8>> = None;
    for entry in archive.entries().context("read tar entries")? {
        let mut entry = entry.context("tar entry")?;
        let path = entry.path().context("entry path")?;
        if path.file_name().and_then(|f| f.to_str()) == Some("relay-node") {
            let mut buf = Vec::new();
            entry
                .read_to_end(&mut buf)
                .context("read binary from tar")?;
            bin_data = Some(buf);
            break;
        }
    }
    let bin_data = bin_data.context("relay-node not found in archive")?;

    let tmp = format!("{DEST}.new");
    std::fs::write(&tmp, &bin_data).with_context(|| format!("write {tmp}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))
            .with_context(|| format!("chmod {tmp}"))?;
    }
    std::fs::rename(&tmp, DEST).with_context(|| format!("rename {tmp} -> {DEST}"))?;
    Ok(())
}

fn validate(tag: &str, amd64_url: &str, arm64_url: &str, sha256_url: &str) -> Result<(), String> {
    if !valid_tag(tag) {
        return Err(format!("malformed tag: {tag}"));
    }
    for url in [amd64_url, arm64_url, sha256_url] {
        if !valid_gh_url(url) {
            return Err(format!("disallowed url host: {url}"));
        }
    }
    Ok(())
}

fn valid_tag(tag: &str) -> bool {
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

fn valid_gh_url(url: &str) -> bool {
    url.starts_with("https://github.com/")
        || url.starts_with("https://objects.githubusercontent.com/")
}

async fn send_report(tx: &mpsc::Sender<NodeMessage>, job_id: i64, state: &str, error: &str) {
    let msg = NodeMessage {
        payload: Some(NodePayload::UpgradeReport(UpgradeReport {
            job_id,
            state: state.to_string(),
            error: error.to_string(),
        })),
    };
    if tx.send(msg).await.is_err() {
        tracing::warn!("upgrade: outbound channel closed before report send");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_bad_url() {
        assert!(validate(
            "v0.2.0",
            "https://evil.example.com/relay-v0.2.0-x86_64-unknown-linux-gnu.tar.gz",
            "https://github.com/foo/bar/releases/download/v0.2.0/relay-v0.2.0-aarch64-unknown-linux-gnu.tar.gz",
            "https://github.com/foo/bar/releases/download/v0.2.0/SHA256SUMS"
        )
        .is_err());
    }

    #[test]
    fn accepts_gh_urls() {
        assert!(validate(
            "v0.2.0",
            "https://github.com/foo/bar/releases/download/v0.2.0/relay-v0.2.0-x86_64-unknown-linux-gnu.tar.gz",
            "https://github.com/foo/bar/releases/download/v0.2.0/relay-v0.2.0-aarch64-unknown-linux-gnu.tar.gz",
            "https://github.com/foo/bar/releases/download/v0.2.0/SHA256SUMS"
        )
        .is_ok());
        assert!(validate(
            "v0.2.0-rc.20260430",
            "https://objects.githubusercontent.com/foo",
            "https://objects.githubusercontent.com/bar",
            "https://github.com/foo/bar/SHA256SUMS"
        )
        .is_ok());
    }

    #[test]
    fn rejects_bad_tag() {
        assert!(!valid_tag("0.2.0"));
        assert!(!valid_tag("v0.2"));
        assert!(!valid_tag("v0.2.0-beta"));
    }
}

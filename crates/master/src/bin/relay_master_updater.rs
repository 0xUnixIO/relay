// relay-master-updater — runs as root, triggered by systemd path unit.
// 零外部工具依赖，始终在退出前删除请求文件。
use std::fs;
use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use flate2::read::GzDecoder;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const REQ: &str = "/var/lib/relay-master/upgrade-request.json";
const STATUS: &str = "/var/lib/relay-master/upgrade-status.json";
const LIB_DIR: &str = "/usr/local/lib/relay-master";
const BIN_LINK: &str = "/usr/local/bin/relay-master";
const SELF_PATH: &str = "/usr/local/lib/relay-master/relay-master-updater";
const UNIT: &str = "relay-master";

#[derive(Deserialize)]
struct UpgradeRequest {
    tag: String,
    asset_url_amd64: String,
    asset_url_arm64: String,
    sha256_url: String,
}

#[derive(Serialize)]
struct UpgradeStatus {
    tag: String,
    state: String,
    error: String,
    completed_at: String,
}

fn utc_now_rfc3339() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let total_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let s = (total_secs % 60) as u32;
    let m = ((total_secs / 60) % 60) as u32;
    let h = ((total_secs / 3600) % 24) as u32;
    let mut days = total_secs / 86400;

    let mut year = 1970u32;
    loop {
        let diy = if is_leap(year) { 366 } else { 365 };
        if days < diy {
            break;
        }
        days -= diy;
        year += 1;
    }
    let month_days = [
        31u64,
        if is_leap(year) { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    let mut month = 1u32;
    for &dm in &month_days {
        if days < dm {
            break;
        }
        days -= dm;
        month += 1;
    }
    let day = (days + 1) as u32;
    format!("{year:04}-{month:02}-{day:02}T{h:02}:{m:02}:{s:02}Z")
}

fn is_leap(y: u32) -> bool {
    y.is_multiple_of(4) && !y.is_multiple_of(100) || y.is_multiple_of(400)
}

fn write_status(tag: &str, state: &str, error: &str) {
    let status = UpgradeStatus {
        tag: tag.to_string(),
        state: state.to_string(),
        error: error.to_string(),
        completed_at: utc_now_rfc3339(),
    };
    if let Ok(json) = serde_json::to_string(&status) {
        let tmp = format!("{STATUS}.tmp");
        if fs::write(&tmp, json.as_bytes()).is_ok() {
            let _ = fs::rename(&tmp, STATUS);
        }
    }
}

fn log(msg: &str) {
    eprintln!("\x1b[1;34m==>\x1b[0m {msg}");
}

fn warn(msg: &str) {
    eprintln!("\x1b[1;33m!!\x1b[0m {msg}");
}

fn is_allowed_url(url: &str) -> bool {
    url.starts_with("https://github.com/")
        || url.starts_with("https://objects.githubusercontent.com/")
}

fn is_valid_tag(tag: &str) -> bool {
    let Some(rest) = tag.strip_prefix('v') else {
        return false;
    };
    let (core, pre) = match rest.find('-') {
        Some(i) => (&rest[..i], Some(&rest[i + 1..])),
        None => (rest, None),
    };
    let parts: Vec<&str> = core.split('.').collect();
    if parts.len() != 3
        || parts
            .iter()
            .any(|p| p.is_empty() || !p.chars().all(|c| c.is_ascii_digit()))
    {
        return false;
    }
    if let Some(pre) = pre {
        if pre.is_empty()
            || !pre
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
        {
            return false;
        }
    }
    true
}

#[derive(Clone, Copy)]
enum Arch {
    Amd64,
    Arm64,
}

fn detect_arch() -> Result<Arch> {
    match std::env::consts::ARCH {
        "x86_64" => Ok(Arch::Amd64),
        "aarch64" => Ok(Arch::Arm64),
        a => bail!("unsupported arch: {a}"),
    }
}

fn target_triple(arch: Arch) -> &'static str {
    match arch {
        Arch::Amd64 => "x86_64-unknown-linux-gnu",
        Arch::Arm64 => "aarch64-unknown-linux-gnu",
    }
}

async fn download(client: &reqwest::Client, url: &str, timeout_secs: u64) -> Result<Vec<u8>> {
    let resp = client
        .get(url)
        .timeout(Duration::from_secs(timeout_secs))
        .send()
        .await
        .with_context(|| format!("GET {url}"))?;
    if !resp.status().is_success() {
        bail!("GET {url} returned {}", resp.status());
    }
    Ok(resp.bytes().await?.to_vec())
}

fn verify_sha256(data: &[u8], sums: &[u8], filename: &str) -> Result<()> {
    let sums_str = std::str::from_utf8(sums).context("SHA256SUMS not valid UTF-8")?;
    let expected = sums_str
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            let (hash, fname) = line.split_once("  ").or_else(|| line.split_once(' '))?;
            (fname.trim() == filename).then(|| hash.trim().to_string())
        })
        .next()
        .with_context(|| format!("{filename} not found in SHA256SUMS"))?;

    let actual = hex::encode(Sha256::digest(data));
    if actual != expected {
        bail!("checksum mismatch: expected {expected}, got {actual}");
    }
    Ok(())
}

fn extract_binary_from_tar(tar_gz: &[u8], name: &str) -> Option<Vec<u8>> {
    let mut archive = tar::Archive::new(GzDecoder::new(tar_gz));
    let entries = archive.entries().ok()?;
    for entry in entries {
        let mut entry = entry.ok()?;
        let path = entry.path().ok()?;
        if path.file_name().and_then(|f| f.to_str()) == Some(name) {
            let mut buf = Vec::new();
            entry.read_to_end(&mut buf).ok()?;
            return Some(buf);
        }
    }
    None
}

fn atomic_write(data: &[u8], dest: &str, mode: u32) -> Result<()> {
    let tmp = format!("{dest}.new");
    fs::write(&tmp, data).with_context(|| format!("write {tmp}"))?;
    fs::set_permissions(&tmp, fs::Permissions::from_mode(mode))
        .with_context(|| format!("chmod {tmp}"))?;
    fs::rename(&tmp, dest).with_context(|| format!("rename {tmp} -> {dest}"))?;
    Ok(())
}

fn atomic_symlink(target: &str, link: &str) -> Result<()> {
    let tmp = format!("{link}.new");
    let _ = fs::remove_file(&tmp);
    std::os::unix::fs::symlink(target, &tmp)
        .with_context(|| format!("symlink {target} -> {tmp}"))?;
    fs::rename(&tmp, link).with_context(|| format!("rename {tmp} -> {link}"))?;
    Ok(())
}

fn systemctl(args: &[&str]) -> bool {
    Command::new("systemctl")
        .args(args)
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn rollback(prev_target: Option<&PathBuf>) {
    warn("rolling back to previous binary");
    if let Some(prev) = prev_target {
        if prev.exists() {
            if let Some(p) = prev.to_str() {
                if let Err(e) = atomic_symlink(p, BIN_LINK) {
                    warn(&format!(
                        "rollback symlink failed: {e:#}; service may stay down"
                    ));
                    return;
                }
            }
        } else {
            warn("previous binary path no longer exists; cannot roll back");
            return;
        }
    }
    if !systemctl(&["restart", UNIT]) {
        warn("rollback restart failed; service may stay down");
    }
}

async fn do_upgrade(req: &UpgradeRequest) -> Result<()> {
    if !is_valid_tag(&req.tag) {
        bail!("malformed tag: {}", req.tag);
    }
    for url in [&req.asset_url_amd64, &req.asset_url_arm64, &req.sha256_url] {
        if !is_allowed_url(url) {
            bail!("URL host not allowed: {url}");
        }
    }
    if !req.sha256_url.ends_with("/SHA256SUMS") {
        bail!("sha256_url must end with /SHA256SUMS");
    }

    let arch = detect_arch()?;
    let target = target_triple(arch);
    let asset_url = match arch {
        Arch::Amd64 => &req.asset_url_amd64,
        Arch::Arm64 => &req.asset_url_arm64,
    };

    let expected_basename = format!("relay-master-{}-{target}.tar.gz", req.tag);
    if !asset_url.ends_with(&format!("/{expected_basename}")) {
        bail!("asset_url basename mismatch (expected {expected_basename})");
    }

    let client = reqwest::Client::builder().build()?;

    log(&format!("downloading {asset_url}"));
    let tar_data = download(&client, asset_url, 300)
        .await
        .context("asset download failed")?;

    log(&format!("downloading {}", req.sha256_url));
    let sums_data = download(&client, &req.sha256_url, 60)
        .await
        .context("SHA256SUMS download failed")?;

    log("verifying SHA256");
    verify_sha256(&tar_data, &sums_data, &expected_basename)?;

    log("extracting relay-master");
    let master_bin = extract_binary_from_tar(&tar_data, "relay-master")
        .context("relay-master not found in archive")?;

    let dest_dir = format!("{LIB_DIR}/relay-master-{}", req.tag);
    fs::create_dir_all(&dest_dir).with_context(|| format!("mkdir -p {dest_dir}"))?;
    let dest_bin = format!("{dest_dir}/relay-master");
    atomic_write(&master_bin, &dest_bin, 0o755).context("install relay-master binary")?;

    if let Some(updater_bin) = extract_binary_from_tar(&tar_data, "relay-master-updater") {
        match atomic_write(&updater_bin, SELF_PATH, 0o755) {
            Ok(()) => log("self-updated relay-master-updater"),
            Err(e) => warn(&format!(
                "failed to self-update relay-master-updater: {e:#}"
            )),
        }
    }

    let prev_target = fs::read_link(BIN_LINK).ok();

    log(&format!("stopping {UNIT}"));
    systemctl(&["stop", UNIT]);

    atomic_symlink(&dest_bin, BIN_LINK).context("symlink swap failed")?;

    log(&format!("starting {UNIT}"));
    if !systemctl(&["start", UNIT]) {
        warn("service failed to start; rolling back");
        rollback(prev_target.as_ref());
        bail!("service failed to start");
    }

    tokio::time::sleep(Duration::from_secs(5)).await;

    if !systemctl(&["is-active", "--quiet", UNIT]) {
        warn("service not active after 5 s; rolling back");
        rollback(prev_target.as_ref());
        bail!("service inactive after restart");
    }

    log(&format!("installed {}", req.tag));
    Ok(())
}

#[tokio::main]
async fn main() {
    if !std::path::Path::new(REQ).exists() {
        warn("no upgrade-request.json; nothing to do");
        return;
    }

    let req_bytes = match fs::read(REQ) {
        Ok(b) => b,
        Err(e) => {
            warn(&format!("read {REQ}: {e}"));
            let _ = fs::remove_file(REQ);
            std::process::exit(1);
        }
    };

    let req: UpgradeRequest = match serde_json::from_slice(&req_bytes) {
        Ok(r) => r,
        Err(e) => {
            warn(&format!("parse {REQ}: {e}"));
            let _ = fs::remove_file(REQ);
            std::process::exit(1);
        }
    };

    let tag = req.tag.clone();
    let result = do_upgrade(&req).await;

    match &result {
        Ok(()) => write_status(&tag, "installed", ""),
        Err(e) => write_status(&tag, "failed", &format!("{e:#}")),
    }
    let _ = fs::remove_file(REQ);

    if let Err(e) = result {
        warn(&format!("upgrade failed: {e:#}"));
        std::process::exit(1);
    }
}

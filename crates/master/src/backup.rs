use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use tokio::sync::Notify;

use crate::snowflake;

pub const R2_CONFIG_KEY: &str = "r2_backup_config";

#[derive(serde::Serialize, serde::Deserialize, Clone, Default)]
pub struct R2BackupConfig {
    pub account_id: String,
    pub bucket_name: String,
    pub access_key_id: String,
    #[serde(default)]
    pub secret_access_key: String,
    #[serde(default)]
    pub path_prefix: String,
    /// 0 = 禁用定时备份；否则每隔 N 小时备份一次
    #[serde(default)]
    pub schedule_hours: u32,
    /// 0 = 不限制保留数量；否则只保留最近 N 份成功备份，自动删除旧的
    #[serde(default)]
    pub keep_count: u32,
}

#[derive(serde::Serialize, sqlx::FromRow)]
pub struct BackupJob {
    pub id: i64,
    pub state: String,
    pub triggered_by: String,
    pub object_key: Option<String>,
    pub size_bytes: Option<i64>,
    pub error: Option<String>,
    pub started_at: chrono::DateTime<Utc>,
    pub completed_at: Option<chrono::DateTime<Utc>>,
}

pub async fn read_r2_config(db: &PgPool) -> Result<Option<R2BackupConfig>, sqlx::Error> {
    let row: Option<(String,)> = sqlx::query_as("SELECT value FROM app_settings WHERE key = $1")
        .bind(R2_CONFIG_KEY)
        .fetch_optional(db)
        .await?;
    Ok(row.and_then(|r| serde_json::from_str(&r.0).ok()))
}

pub fn spawn(db: PgPool, trigger: Arc<Notify>) {
    tokio::spawn(async move {
        loop {
            let wait = match read_r2_config(&db).await {
                Ok(Some(c)) if !c.account_id.is_empty() && c.schedule_hours > 0 => {
                    Duration::from_secs(c.schedule_hours as u64 * 3600)
                }
                _ => Duration::from_secs(3600),
            };

            tokio::select! {
                _ = trigger.notified() => {
                    do_backup(&db, "manual").await;
                }
                _ = tokio::time::sleep(wait) => {
                    match read_r2_config(&db).await {
                        Ok(Some(c)) if !c.account_id.is_empty() && c.schedule_hours > 0 => {
                            do_backup(&db, "schedule").await;
                        }
                        _ => {}
                    }
                }
            }
        }
    });
}

async fn do_backup(db: &PgPool, triggered_by: &str) {
    let cfg = match read_r2_config(db).await {
        Ok(Some(c)) if !c.account_id.is_empty() => c,
        _ => {
            tracing::warn!("backup triggered but R2 config is missing or incomplete");
            return;
        }
    };

    let job_id = snowflake::next_id();
    if let Err(e) =
        sqlx::query("INSERT INTO backup_jobs (id, state, triggered_by) VALUES ($1, 'running', $2)")
            .bind(job_id)
            .bind(triggered_by)
            .execute(db)
            .await
    {
        tracing::error!(error = %e, "failed to insert backup_jobs row");
        return;
    }

    match export_and_upload(db, &cfg).await {
        Ok((object_key, size_bytes)) => {
            let _ = sqlx::query(
                "UPDATE backup_jobs
                    SET state='succeeded', object_key=$1, size_bytes=$2, completed_at=now()
                  WHERE id=$3",
            )
            .bind(&object_key)
            .bind(size_bytes as i64)
            .bind(job_id)
            .execute(db)
            .await;
            tracing::info!(object_key, size_bytes, triggered_by, "backup succeeded");
            if cfg.keep_count > 0 {
                if let Err(e) = prune_old_backups(db, &cfg).await {
                    tracing::warn!(error = %e, "backup pruning failed");
                }
            }
        }
        Err(e) => {
            let _ = sqlx::query(
                "UPDATE backup_jobs
                    SET state='failed', error=$1, completed_at=now()
                  WHERE id=$2",
            )
            .bind(e.to_string())
            .bind(job_id)
            .execute(db)
            .await;
            tracing::error!(error = %e, triggered_by, "backup failed");
        }
    }
}

/// 历史/日志表，数据量大且对业务恢复无意义，默认排除
const EXCLUDED_TABLES: &[&str] = &["node_availability", "audit_log"];

/// 将双引号转义为 PostgreSQL 合法的引用标识符
fn pg_quote_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

async fn export_and_upload(db: &PgPool, cfg: &R2BackupConfig) -> anyhow::Result<(String, usize)> {
    // 1. 获取所有用户表（排除大体积历史表）
    let tables: Vec<(String,)> = sqlx::query_as(
        "SELECT table_name FROM information_schema.tables
          WHERE table_schema = 'public' AND table_type = 'BASE TABLE'
            AND table_name != ALL($1)
          ORDER BY table_name",
    )
    .bind(EXCLUDED_TABLES)
    .fetch_all(db)
    .await?;

    // 2. 逐表导出为 JSON（row_to_json 转 text，避免依赖 sqlx json feature）
    let mut table_map = serde_json::Map::new();
    for (table_name,) in &tables {
        let safe = pg_quote_ident(table_name);
        let rows: Vec<serde_json::Value> = sqlx::query_scalar::<_, String>(&format!(
            "SELECT row_to_json(t)::text FROM (SELECT * FROM {safe}) t"
        ))
        .fetch_all(db)
        .await?
        .into_iter()
        .map(|s| serde_json::from_str(&s).unwrap_or(serde_json::Value::Null))
        .collect();
        table_map.insert(table_name.clone(), serde_json::Value::Array(rows));
    }

    let payload = serde_json::json!({
        "version": 1,
        "created_at": Utc::now().to_rfc3339(),
        "tables": table_map,
    });

    // 3. Gzip 压缩
    use flate2::{write::GzEncoder, Compression};
    use std::io::Write as _;
    let json_bytes = serde_json::to_vec(&payload)?;
    let mut enc = GzEncoder::new(Vec::new(), Compression::default());
    enc.write_all(&json_bytes)?;
    let compressed = enc.finish()?;
    let size = compressed.len();

    // 4. 构造对象键并上传
    let ts = Utc::now().format("%Y%m%d-%H%M%S");
    let prefix = if cfg.path_prefix.is_empty() {
        String::new()
    } else {
        format!("{}/", cfg.path_prefix.trim_end_matches('/'))
    };
    let object_key = format!("{prefix}relay-backup-{ts}.json.gz");

    upload_to_r2(cfg, &object_key, &compressed).await?;

    Ok((object_key, size))
}

async fn prune_old_backups(db: &PgPool, cfg: &R2BackupConfig) -> anyhow::Result<()> {
    let old: Vec<(i64, String)> = sqlx::query_as(
        "SELECT id, object_key FROM backup_jobs
          WHERE state = 'succeeded' AND object_key IS NOT NULL
          ORDER BY started_at DESC
          OFFSET $1",
    )
    .bind(cfg.keep_count as i64)
    .fetch_all(db)
    .await?;

    for (id, key) in old {
        if let Err(e) = delete_from_r2(cfg, &key).await {
            tracing::warn!(key, error = %e, "failed to delete old backup from R2, skipping");
            continue;
        }
        let _ = sqlx::query("DELETE FROM backup_jobs WHERE id = $1")
            .bind(id)
            .execute(db)
            .await;
        tracing::info!(key, "pruned old backup");
    }
    Ok(())
}

async fn delete_from_r2(cfg: &R2BackupConfig, object_key: &str) -> anyhow::Result<()> {
    let now = Utc::now();
    let date_str = now.format("%Y%m%d").to_string();
    let datetime_str = now.format("%Y%m%dT%H%M%SZ").to_string();

    let region = "auto";
    let host = format!("{}.r2.cloudflarestorage.com", cfg.account_id);
    let url = format!("https://{}/{}/{}", host, cfg.bucket_name, object_key);

    let payload_hash = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
    let encoded_key = uri_encode_path(object_key);
    let encoded_bucket = uri_encode(cfg.bucket_name.as_str());
    let canonical_uri = format!("/{}/{}", encoded_bucket, encoded_key);

    let canonical_headers =
        format!("host:{host}\nx-amz-content-sha256:{payload_hash}\nx-amz-date:{datetime_str}\n");
    let signed_headers = "host;x-amz-content-sha256;x-amz-date";
    let canonical_request =
        format!("DELETE\n{canonical_uri}\n\n{canonical_headers}\n{signed_headers}\n{payload_hash}");

    let credential_scope = format!("{date_str}/{region}/s3/aws4_request");
    let string_to_sign = format!(
        "AWS4-HMAC-SHA256\n{datetime_str}\n{credential_scope}\n{}",
        hex::encode(Sha256::digest(canonical_request.as_bytes()))
    );

    let k_date = hmac_sha256(
        format!("AWS4{}", cfg.secret_access_key).as_bytes(),
        date_str.as_bytes(),
    );
    let k_region = hmac_sha256(&k_date, region.as_bytes());
    let k_service = hmac_sha256(&k_region, b"s3");
    let k_signing = hmac_sha256(&k_service, b"aws4_request");
    let signature = hex::encode(hmac_sha256(&k_signing, string_to_sign.as_bytes()));

    let auth = format!(
        "AWS4-HMAC-SHA256 Credential={}/{},SignedHeaders={},Signature={}",
        cfg.access_key_id, credential_scope, signed_headers, signature
    );

    let client = reqwest::Client::new();
    let resp = client
        .delete(&url)
        .header("x-amz-date", &datetime_str)
        .header("x-amz-content-sha256", payload_hash)
        .header("authorization", auth)
        .send()
        .await?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(anyhow::anyhow!("R2 删除失败 ({status}): {body}"));
    }
    Ok(())
}

// ---------- AWS SigV4 (S3-compatible) ----------

fn hmac_sha256(key: &[u8], msg: &[u8]) -> [u8; 32] {
    const BLOCK: usize = 64;
    let mut k = [0u8; BLOCK];
    if key.len() > BLOCK {
        let h = Sha256::digest(key);
        k[..32].copy_from_slice(&h);
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let mut ipad = k;
    let mut opad = k;
    for i in 0..BLOCK {
        ipad[i] ^= 0x36;
        opad[i] ^= 0x5c;
    }
    let inner = Sha256::new()
        .chain_update(ipad)
        .chain_update(msg)
        .finalize();
    Sha256::new()
        .chain_update(opad)
        .chain_update(inner)
        .finalize()
        .into()
}

async fn upload_to_r2(cfg: &R2BackupConfig, object_key: &str, data: &[u8]) -> anyhow::Result<()> {
    let now = Utc::now();
    let date_str = now.format("%Y%m%d").to_string();
    let datetime_str = now.format("%Y%m%dT%H%M%SZ").to_string();

    let region = "auto";
    let host = format!("{}.r2.cloudflarestorage.com", cfg.account_id);
    let url = format!("https://{}/{}/{}", host, cfg.bucket_name, object_key);

    let payload_hash = hex::encode(Sha256::digest(data));

    // 规范化 URI：path-style，每个段分别做 percent-encode（不编码 /-_.~字母数字）
    let encoded_key = uri_encode_path(object_key);
    let encoded_bucket = uri_encode(cfg.bucket_name.as_str());
    let canonical_uri = format!("/{}/{}", encoded_bucket, encoded_key);

    let canonical_headers = format!(
        "content-type:application/octet-stream\nhost:{host}\nx-amz-content-sha256:{payload_hash}\nx-amz-date:{datetime_str}\n"
    );
    let signed_headers = "content-type;host;x-amz-content-sha256;x-amz-date";

    let canonical_request =
        format!("PUT\n{canonical_uri}\n\n{canonical_headers}\n{signed_headers}\n{payload_hash}");

    let credential_scope = format!("{date_str}/{region}/s3/aws4_request");
    let string_to_sign = format!(
        "AWS4-HMAC-SHA256\n{datetime_str}\n{credential_scope}\n{}",
        hex::encode(Sha256::digest(canonical_request.as_bytes()))
    );

    let k_date = hmac_sha256(
        format!("AWS4{}", cfg.secret_access_key).as_bytes(),
        date_str.as_bytes(),
    );
    let k_region = hmac_sha256(&k_date, region.as_bytes());
    let k_service = hmac_sha256(&k_region, b"s3");
    let k_signing = hmac_sha256(&k_service, b"aws4_request");
    let signature = hex::encode(hmac_sha256(&k_signing, string_to_sign.as_bytes()));

    let auth = format!(
        "AWS4-HMAC-SHA256 Credential={}/{},SignedHeaders={},Signature={}",
        cfg.access_key_id, credential_scope, signed_headers, signature
    );

    let client = reqwest::Client::new();
    let resp = client
        .put(&url)
        .header("content-type", "application/octet-stream")
        .header("x-amz-date", &datetime_str)
        .header("x-amz-content-sha256", &payload_hash)
        .header("authorization", auth)
        .body(data.to_vec())
        .send()
        .await?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(anyhow::anyhow!("R2 上传失败 ({status}): {body}"));
    }

    Ok(())
}

async fn download_from_r2(cfg: &R2BackupConfig, object_key: &str) -> anyhow::Result<Vec<u8>> {
    let now = Utc::now();
    let date_str = now.format("%Y%m%d").to_string();
    let datetime_str = now.format("%Y%m%dT%H%M%SZ").to_string();

    let region = "auto";
    let host = format!("{}.r2.cloudflarestorage.com", cfg.account_id);
    let url = format!("https://{}/{}/{}", host, cfg.bucket_name, object_key);

    // GET 请求无请求体，payload hash 为空字符串的 SHA256
    let payload_hash = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

    let encoded_key = uri_encode_path(object_key);
    let encoded_bucket = uri_encode(cfg.bucket_name.as_str());
    let canonical_uri = format!("/{}/{}", encoded_bucket, encoded_key);

    let canonical_headers =
        format!("host:{host}\nx-amz-content-sha256:{payload_hash}\nx-amz-date:{datetime_str}\n");
    let signed_headers = "host;x-amz-content-sha256;x-amz-date";

    let canonical_request =
        format!("GET\n{canonical_uri}\n\n{canonical_headers}\n{signed_headers}\n{payload_hash}");

    let credential_scope = format!("{date_str}/{region}/s3/aws4_request");
    let string_to_sign = format!(
        "AWS4-HMAC-SHA256\n{datetime_str}\n{credential_scope}\n{}",
        hex::encode(Sha256::digest(canonical_request.as_bytes()))
    );

    let k_date = hmac_sha256(
        format!("AWS4{}", cfg.secret_access_key).as_bytes(),
        date_str.as_bytes(),
    );
    let k_region = hmac_sha256(&k_date, region.as_bytes());
    let k_service = hmac_sha256(&k_region, b"s3");
    let k_signing = hmac_sha256(&k_service, b"aws4_request");
    let signature = hex::encode(hmac_sha256(&k_signing, string_to_sign.as_bytes()));

    let auth = format!(
        "AWS4-HMAC-SHA256 Credential={}/{},SignedHeaders={},Signature={}",
        cfg.access_key_id, credential_scope, signed_headers, signature
    );

    let client = reqwest::Client::new();
    let resp = client
        .get(&url)
        .header("x-amz-date", &datetime_str)
        .header("x-amz-content-sha256", payload_hash)
        .header("authorization", auth)
        .send()
        .await?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(anyhow::anyhow!("R2 下载失败 ({status}): {body}"));
    }

    Ok(resp.bytes().await?.to_vec())
}

/// R2 中实际存在的备份对象
#[derive(serde::Serialize)]
pub struct R2BackupFile {
    pub key: String,
    pub size: i64,
    pub last_modified: chrono::DateTime<Utc>,
}

pub async fn list_objects_from_r2(cfg: &R2BackupConfig) -> anyhow::Result<Vec<R2BackupFile>> {
    let now = Utc::now();
    let date_str = now.format("%Y%m%d").to_string();
    let datetime_str = now.format("%Y%m%dT%H%M%SZ").to_string();

    let region = "auto";
    let host = format!("{}.r2.cloudflarestorage.com", cfg.account_id);
    let payload_hash = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

    let encoded_bucket = uri_encode(&cfg.bucket_name);
    let canonical_uri = format!("/{encoded_bucket}");

    // 构造查询参数（必须按字母顺序排列）
    let prefix = if cfg.path_prefix.is_empty() {
        String::new()
    } else {
        format!("{}/", cfg.path_prefix.trim_end_matches('/'))
    };
    let canonical_qs = if prefix.is_empty() {
        "list-type=2&max-keys=1000".to_string()
    } else {
        format!("list-type=2&max-keys=1000&prefix={}", uri_encode(&prefix))
    };
    let url = format!("https://{host}{canonical_uri}?{canonical_qs}");

    let canonical_headers =
        format!("host:{host}\nx-amz-content-sha256:{payload_hash}\nx-amz-date:{datetime_str}\n");
    let signed_headers = "host;x-amz-content-sha256;x-amz-date";
    let canonical_request =
        format!("GET\n{canonical_uri}\n{canonical_qs}\n{canonical_headers}\n{signed_headers}\n{payload_hash}");

    let credential_scope = format!("{date_str}/{region}/s3/aws4_request");
    let string_to_sign = format!(
        "AWS4-HMAC-SHA256\n{datetime_str}\n{credential_scope}\n{}",
        hex::encode(Sha256::digest(canonical_request.as_bytes()))
    );

    let k_date = hmac_sha256(
        format!("AWS4{}", cfg.secret_access_key).as_bytes(),
        date_str.as_bytes(),
    );
    let k_region = hmac_sha256(&k_date, region.as_bytes());
    let k_service = hmac_sha256(&k_region, b"s3");
    let k_signing = hmac_sha256(&k_service, b"aws4_request");
    let signature = hex::encode(hmac_sha256(&k_signing, string_to_sign.as_bytes()));

    let auth = format!(
        "AWS4-HMAC-SHA256 Credential={}/{},SignedHeaders={},Signature={}",
        cfg.access_key_id, credential_scope, signed_headers, signature
    );

    let client = reqwest::Client::new();
    let resp = client
        .get(&url)
        .header("x-amz-date", &datetime_str)
        .header("x-amz-content-sha256", payload_hash)
        .header("authorization", auth)
        .send()
        .await?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(anyhow::anyhow!("R2 ListObjects 失败 ({status}): {body}"));
    }

    let xml = resp.text().await?;
    Ok(parse_list_objects_xml(&xml))
}

fn parse_list_objects_xml(xml: &str) -> Vec<R2BackupFile> {
    let mut objects = Vec::new();
    let mut rest = xml;
    while let Some(start) = rest.find("<Contents>") {
        rest = &rest[start + "<Contents>".len()..];
        let end = rest.find("</Contents>").unwrap_or(rest.len());
        let block = &rest[..end];

        let key = xml_text(block, "Key").unwrap_or("").to_string();
        let size = xml_text(block, "Size")
            .and_then(|s| s.parse::<i64>().ok())
            .unwrap_or(0);
        let last_modified = xml_text(block, "LastModified")
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.with_timezone(&Utc))
            .unwrap_or_else(Utc::now);

        if !key.is_empty() {
            objects.push(R2BackupFile {
                key,
                size,
                last_modified,
            });
        }
        rest = &rest[end..];
    }
    objects
}

fn xml_text<'a>(block: &'a str, tag: &str) -> Option<&'a str> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = block.find(&open)? + open.len();
    let end = block[start..].find(close.as_str())?;
    Some(&block[start..start + end])
}

/// 查询 FK 依赖并做拓扑排序，返回"父表在前、子表在后"的插入顺序。
/// 不在 `tables` 列表中的依赖会被忽略（如已排除的历史表）。
async fn topo_sort_by_fk(db: &PgPool, tables: &[&str]) -> anyhow::Result<Vec<String>> {
    use std::collections::{HashMap, HashSet, VecDeque};

    // child -> [parents]（仅限本次恢复的表）
    let table_set: HashSet<&str> = tables.iter().copied().collect();

    let deps: Vec<(String, String)> = sqlx::query_as(
        "SELECT DISTINCT
                kcu.table_name  AS child,
                ccu.table_name  AS parent
           FROM information_schema.table_constraints    tc
           JOIN information_schema.key_column_usage     kcu
             ON tc.constraint_name   = kcu.constraint_name
            AND tc.table_schema      = kcu.table_schema
           JOIN information_schema.referential_constraints rc
             ON tc.constraint_name   = rc.constraint_name
            AND tc.table_schema      = rc.constraint_schema
           JOIN information_schema.constraint_column_usage ccu
             ON rc.unique_constraint_name   = ccu.constraint_name
            AND rc.unique_constraint_schema = ccu.table_schema
          WHERE tc.constraint_type = 'FOREIGN KEY'
            AND tc.table_schema    = 'public'",
    )
    .fetch_all(db)
    .await?;

    // in-degree 和邻接表（只含目标表之间的边）
    let mut in_degree: HashMap<&str, usize> = tables.iter().map(|&t| (t, 0)).collect();
    let mut children: HashMap<&str, Vec<&str>> = HashMap::new();

    for (child, parent) in &deps {
        let c = child.as_str();
        let p = parent.as_str();
        if table_set.contains(c) && table_set.contains(p) && c != p {
            *in_degree.entry(c).or_insert(0) += 1;
            children.entry(p).or_default().push(c);
        }
    }

    // Kahn 算法
    let mut queue: VecDeque<&str> = in_degree
        .iter()
        .filter(|(_, &d)| d == 0)
        .map(|(&t, _)| t)
        .collect();
    let mut sorted = Vec::with_capacity(tables.len());

    while let Some(t) = queue.pop_front() {
        sorted.push(t.to_string());
        if let Some(kids) = children.get(t) {
            for &k in kids {
                let deg = in_degree.entry(k).or_insert(0);
                *deg -= 1;
                if *deg == 0 {
                    queue.push_back(k);
                }
            }
        }
    }

    // 若存在循环依赖，将剩余表追加到末尾（不应出现，但作保底）
    if sorted.len() < tables.len() {
        for &t in tables {
            if !sorted.contains(&t.to_string()) {
                sorted.push(t.to_string());
            }
        }
    }

    Ok(sorted)
}

pub async fn restore_from_r2(
    db: &PgPool,
    cfg: &R2BackupConfig,
    object_key: &str,
) -> anyhow::Result<()> {
    let compressed = download_from_r2(cfg, object_key).await?;

    use flate2::read::GzDecoder;
    use std::io::Read;
    let mut gz = GzDecoder::new(&compressed[..]);
    let mut json_bytes = Vec::new();
    gz.read_to_end(&mut json_bytes)?;

    let backup: serde_json::Value = serde_json::from_slice(&json_bytes)?;
    let version = backup["version"].as_i64().unwrap_or(0);
    if version != 1 {
        return Err(anyhow::anyhow!("不支持的备份版本: {version}"));
    }
    let tables = backup["tables"]
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("备份格式错误：缺少 tables 字段"))?;

    let non_empty_tables: Vec<&str> = tables
        .keys()
        .filter(|t| !tables[*t].as_array().map(|a| a.is_empty()).unwrap_or(true))
        .map(|s| s.as_str())
        .collect();

    // 按 FK 依赖拓扑排序，保证父表先于子表插入，无需 superuser 权限
    let ordered = topo_sort_by_fk(db, &non_empty_tables).await?;

    let mut tx = db.begin().await?;

    // TRUNCATE CASCADE 一次性清空（不需要特殊权限）
    let table_list: Vec<String> = non_empty_tables.iter().map(|t| pg_quote_ident(t)).collect();
    if !table_list.is_empty() {
        let sql = format!("TRUNCATE TABLE {} CASCADE", table_list.join(", "));
        sqlx::query(&sql).execute(&mut *tx).await?;
    }

    // 按拓扑顺序写回，满足 FK 约束
    for table_name in &ordered {
        let rows = match tables.get(table_name) {
            Some(r) if !r.as_array().map(|a| a.is_empty()).unwrap_or(true) => r,
            _ => continue,
        };

        // 从备份的第一行提取字段名，只 INSERT 备份中存在的列，
        // 让数据库对新增的 NOT NULL DEFAULT 列自动套用默认值
        let col_names: Vec<String> = rows
            .as_array()
            .and_then(|a| a.first())
            .and_then(|v| v.as_object())
            .map(|obj| obj.keys().map(|k| pg_quote_ident(k)).collect())
            .unwrap_or_default();

        if col_names.is_empty() {
            continue;
        }

        let safe = pg_quote_ident(table_name);
        let col_list = col_names.join(", ");
        let rows_json = serde_json::to_string(rows)?;
        sqlx::query(&format!(
            "INSERT INTO {safe} ({col_list})
             SELECT {col_list} FROM json_populate_recordset(null::{safe}, $1::json)"
        ))
        .bind(&rows_json)
        .execute(&mut *tx)
        .await?;
    }

    tx.commit().await?;
    tracing::info!(object_key, "backup restore completed");
    Ok(())
}

fn uri_encode(s: &str) -> String {
    s.chars()
        .flat_map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' || c == '~' {
                vec![c]
            } else {
                let mut buf = [0u8; 4];
                let encoded = c.encode_utf8(&mut buf);
                encoded
                    .bytes()
                    .flat_map(|b| format!("%{b:02X}").chars().collect::<Vec<_>>())
                    .collect()
            }
        })
        .collect()
}

/// 对 object key 中的每个路径段分别 encode，但保留 `/`
fn uri_encode_path(path: &str) -> String {
    path.split('/')
        .map(uri_encode)
        .collect::<Vec<_>>()
        .join("/")
}

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Instant;

use chrono::{DateTime, Utc};
use redis::aio::ConnectionManager;
use sqlx::PgPool;
use tokio::sync::{Mutex, Notify, RwLock};

use crate::config::Config;
use crate::models::Node;
use crate::pki::Pki;
use crate::registry::NodeRegistry;
use crate::series::{CounterDeltas, SeriesStore};
use crate::upgrade::UpgradeResolver;

#[derive(Clone)]
pub struct SetupEntry {
    pub node_id: String,
    pub enrollment_token: String,
    pub master_endpoint: String,
    pub enroll_endpoint: String,
    pub ca_cert_b64: String,
    pub created_at: Instant,
}

/// 节点心跳运行时（in-process L1 cache，单 master 真相源）。
#[derive(Clone, Debug)]
pub struct NodeRuntimeEntry {
    pub last_heartbeat: serde_json::Value,
    pub last_seen_at: DateTime<Utc>,
    pub version: String,
    pub protocol_version: i32,
    pub capabilities: Vec<String>,
    /// 上次写入 Postgres 的时间，用于节流（默认每节点每 30s 一次）。
    pub last_pg_write_at: DateTime<Utc>,
}

#[derive(Clone)]
pub struct AppState {
    pub cfg: Arc<Config>,
    pub db: PgPool,
    pub registry: NodeRegistry,
    pub series: SeriesStore,
    pub counter_deltas: CounterDeltas,
    pub pki: Arc<Pki>,
    /// 可选 Redis 连接管理器；用于 probe 防抖等轻量缓存。
    /// 未配置 / 暂时连不上时为 `None`，调用方需 fail-open。
    pub redis: Option<ConnectionManager>,
    /// L1 心跳运行时。读路径优先此处，PG 仅作冷备。
    pub node_runtime: Arc<RwLock<HashMap<String, NodeRuntimeEntry>>>,
    /// node_availability 每节点每分钟去重，避免每条心跳都打 PG。
    /// key = (node_id, unix_minute)。
    pub availability_seen: Arc<Mutex<HashSet<(String, i64)>>>,
    /// GitHub releases resolver + cache for the upgrade feature.
    pub upgrade_resolver: UpgradeResolver,
    /// 手动触发备份的通知信道。
    pub backup_trigger: Arc<Notify>,
    /// setup token → 安装参数，有效期 24h。
    pub setup_tokens: Arc<RwLock<HashMap<String, SetupEntry>>>,
}

impl AppState {
    pub fn new(cfg: Config, db: PgPool, pki: Arc<Pki>, redis: Option<ConnectionManager>) -> Self {
        Self {
            cfg: Arc::new(cfg),
            db,
            registry: NodeRegistry::new(),
            series: SeriesStore::new(),
            counter_deltas: CounterDeltas::new(),
            pki,
            redis,
            node_runtime: Arc::new(RwLock::new(HashMap::new())),
            availability_seen: Arc::new(Mutex::new(HashSet::new())),
            upgrade_resolver: UpgradeResolver::new(crate::upgrade::DEFAULT_REPO),
            backup_trigger: Arc::new(Notify::new()),
            setup_tokens: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// 用 L1 心跳数据覆盖 Node 的 runtime 字段（last_heartbeat / last_seen_at /
    /// version / protocol_version）。L1 缺失时保留 PG 字段不变。
    pub async fn overlay_node(&self, n: &mut Node) {
        let map = self.node_runtime.read().await;
        if let Some(e) = map.get(&n.id) {
            n.last_heartbeat = Some(e.last_heartbeat.clone());
            n.last_seen_at = Some(e.last_seen_at);
            if !e.version.is_empty() {
                n.version = e.version.clone();
            }
            n.protocol_version = e.protocol_version;
            n.capabilities = e.capabilities.clone();
        }
    }

    pub async fn overlay_nodes(&self, nodes: &mut [Node]) {
        let map = self.node_runtime.read().await;
        for n in nodes.iter_mut() {
            if let Some(e) = map.get(&n.id) {
                n.last_heartbeat = Some(e.last_heartbeat.clone());
                n.last_seen_at = Some(e.last_seen_at);
                if !e.version.is_empty() {
                    n.version = e.version.clone();
                }
                n.protocol_version = e.protocol_version;
                n.capabilities = e.capabilities.clone();
            }
        }
    }
}

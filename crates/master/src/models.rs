use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct User {
    #[serde(with = "crate::snowflake::as_str")]
    pub id: i64,
    pub username: String,
    #[serde(skip_serializing)]
    pub password_hash: String,
    pub role: String,
    pub status: String,
    pub expires_at: Option<DateTime<Utc>>,
    pub remark: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Node {
    pub id: String,
    pub hostname: String,
    pub version: String,
    #[serde(default)]
    pub protocol_version: i32,
    pub tags: Vec<String>,
    #[serde(skip_serializing)]
    #[allow(dead_code)]
    pub enrollment_token: Option<String>,
    pub enrolled_at: Option<DateTime<Utc>>,
    pub last_seen_at: Option<DateTime<Utc>>,
    pub last_heartbeat: Option<serde_json::Value>,
    pub cert_fingerprint: Option<String>,
    pub cert_serial: Option<String>,
    pub cert_not_after: Option<DateTime<Utc>>,
    pub server_ips: Vec<String>,
    pub port_range_start: i32,
    pub port_range_end: i32,
    pub traffic_ratio: f64,
    pub tunnel_eligible: bool,
    pub expires_at: Option<DateTime<Utc>>,
    pub monthly_price: Option<f64>,
    pub website: String,
    pub listen_stack: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// Runtime-only: heartbeat-advertised capability tokens. Not persisted
    /// (no DB column); populated via overlay_node/overlay_nodes.
    #[sqlx(default)]
    #[serde(default)]
    pub capabilities: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Tunnel {
    #[serde(with = "crate::snowflake::as_str")]
    pub id: i64,
    pub name: String,
    pub description: String,
    pub protocols: Vec<String>,
    pub ip_preference: String,
    pub in_ip: String,
    pub enabled: bool,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TunnelView {
    #[serde(flatten)]
    pub tunnel: Tunnel,
    /// Flat hop listing (one entry per node × hop). Preserved for legacy
    /// clients; ordered by (hop_index, node_id).
    pub hops: Vec<TunnelHopRef>,
    /// Layered DAG view: `layers[i]` is the list of node IDs at hop_index = i.
    /// Single-node-per-hop tunnels yield `layers.len() == hops.len()`.
    #[serde(default)]
    pub layers: Vec<Vec<String>>,
    /// True iff any layer hosts more than one node.
    #[serde(default)]
    pub is_layered: bool,
    pub user_tunnel_count: i64,
    pub forward_count: i64,
    /// node_id → hostname 映射，供前端显示节点名称
    #[serde(default)]
    pub node_names: std::collections::HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TunnelHopRef {
    pub hop_index: i32,
    pub node_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct UserTunnel {
    #[serde(with = "crate::snowflake::as_str")]
    pub id: i64,
    #[serde(with = "crate::snowflake::as_str")]
    pub user_id: i64,
    #[serde(with = "crate::snowflake::as_str")]
    pub tunnel_id: i64,
    pub flow_limit_bytes: i64,
    pub speed_limit_kbps: i64,
    pub expires_at: Option<DateTime<Utc>>,
    pub enabled: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserTunnelView {
    #[serde(flatten)]
    pub user_tunnel: UserTunnel,
    pub username: String,
    pub tunnel_name: String,
    pub tunnel_protocols: Vec<String>,
    pub in_flow_bytes: i64,
    pub out_flow_bytes: i64,
    pub forward_count: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Forward {
    #[serde(with = "crate::snowflake::as_str")]
    pub id: i64,
    #[serde(with = "crate::snowflake::as_str")]
    pub user_tunnel_id: i64,
    pub name: String,
    pub in_port: i32,
    pub remote_addrs: Vec<String>,
    pub lb_strategy: String,
    pub max_connections: i32,
    pub allow_cidrs: Vec<String>,
    pub deny_cidrs: Vec<String>,
    pub desired_enabled: bool,
    pub deploy_generation: i64,
    pub in_flow_bytes: i64,
    pub out_flow_bytes: i64,
    pub last_deploy_error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct ForwardPort {
    #[serde(with = "crate::snowflake::as_str")]
    pub forward_id: i64,
    pub hop_index: i32,
    pub node_id: String,
    pub protocol: String,
    pub listen_port: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ForwardView {
    #[serde(flatten)]
    pub forward: Forward,
    #[serde(with = "crate::snowflake::as_str")]
    pub user_id: i64,
    pub username: String,
    #[serde(with = "crate::snowflake::as_str")]
    pub tunnel_id: i64,
    pub tunnel_name: String,
    pub protocols: Vec<String>,
    pub ports: Vec<ForwardPort>,
    pub effective_enabled: bool,
    pub pause_reasons: Vec<String>,
    #[serde(default)]
    pub active_connections: u32,
    pub entry_addr: Option<String>,
    /// All entry-layer addresses (multiple when entry layer has multiple
    /// nodes for DNS-LB). Empty when no entry hop.
    #[serde(default)]
    pub entry_addrs: Vec<String>,
    /// 当 lb_strategy='best' 时，当前评分最优的上游地址（nil 表示暂无探测数据）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub best_exit_addr: Option<String>,
    /// 当前活跃客户端 IP 列表（内存快照，随 ForwardStats 更新，最多滞后 30s）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub active_client_ips: Vec<String>,
    /// 所有跳节点均已回执当前配置版本，实时计算。
    pub synced: bool,
}

// ---------- User Groups ----------

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct UserGroup {
    #[serde(with = "crate::snowflake::as_str")]
    pub id: i64,
    pub name: String,
    pub remark: String,
    pub flow_limit_bytes: i64,
    pub speed_limit_kbps: i64,
    pub forward_limit: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserGroupView {
    #[serde(flatten)]
    pub group: UserGroup,
    pub member_count: i64,
    pub tunnel_count: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroupMemberView {
    #[serde(with = "crate::snowflake::as_str")]
    pub user_id: i64,
    pub username: String,
    pub role: String,
    pub status: String,
    pub added_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct GroupTunnel {
    #[serde(with = "crate::snowflake::as_str")]
    pub id: i64,
    #[serde(with = "crate::snowflake::as_str")]
    pub group_id: i64,
    #[serde(with = "crate::snowflake::as_str")]
    pub tunnel_id: i64,
    pub enabled: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroupTunnelView {
    #[serde(flatten)]
    pub group_tunnel: GroupTunnel,
    pub tunnel_name: String,
    pub tunnel_protocols: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct SystemConfig {
    pub announcement_enabled: bool,
    pub announcement_title: String,
    pub announcement_content: String,
    pub updated_at: DateTime<Utc>,
    /// 监控数据（forward_stats / node_heartbeats）保留天数，1–3650，默认 30。
    pub monitor_retention_days: i32,
}

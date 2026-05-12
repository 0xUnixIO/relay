//! In-memory registry of currently-connected nodes and a latest-wins
//! config push channel for each. Config snapshots are versioned per node
//! so the node can ignore stale arrivals.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use sqlx::PgPool;
use tokio::sync::{mpsc, oneshot, watch, Mutex, RwLock};
use tonic::Status;

use relay_proto::v1::{
    master_message::Payload as MasterPayload, ConfigUpdate, ForwardConfig, MasterMessage,
    ProbeRequest, ProbeResult,
};

static CONN_SEQ: AtomicU64 = AtomicU64::new(1);
static PROBE_SEQ: AtomicU64 = AtomicU64::new(1);

#[derive(Clone)]
pub struct NodeRegistry {
    inner: Arc<RwLock<HashMap<String, NodeConn>>>,
    pending_probes: Arc<Mutex<HashMap<u64, oneshot::Sender<ProbeResult>>>>,
}

#[derive(Clone)]
pub struct NodeConn {
    pub conn_id: u64,
    pub config_tx: watch::Sender<Option<ConfigUpdate>>,
    pub out_tx: mpsc::Sender<Result<MasterMessage, Status>>,
}

pub struct NodeSubscription {
    pub conn_id: u64,
    pub node_id: String,
    pub config_rx: watch::Receiver<Option<ConfigUpdate>>,
    registry: NodeRegistry,
}

impl Drop for NodeSubscription {
    fn drop(&mut self) {
        let registry = self.registry.clone();
        let node_id = self.node_id.clone();
        let conn_id = self.conn_id;
        tokio::spawn(async move {
            let mut g = registry.inner.write().await;
            if let Some(c) = g.get(&node_id) {
                if c.conn_id == conn_id {
                    g.remove(&node_id);
                    tracing::info!(%node_id, conn_id, "channel removed from registry");
                }
            }
        });
    }
}

impl NodeRegistry {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(HashMap::new())),
            pending_probes: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Open a new subscription, displacing any prior one for this node.
    pub async fn subscribe(
        &self,
        node_id: String,
        out_tx: mpsc::Sender<Result<MasterMessage, Status>>,
    ) -> NodeSubscription {
        let conn_id = CONN_SEQ.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = watch::channel(None);
        let mut g = self.inner.write().await;
        g.insert(
            node_id.clone(),
            NodeConn {
                conn_id,
                config_tx: tx,
                out_tx,
            },
        );
        tracing::info!(%node_id, conn_id, "channel registered");
        NodeSubscription {
            conn_id,
            node_id,
            config_rx: rx,
            registry: self.clone(),
        }
    }

    /// Build the current ConfigUpdate for `node_id` from DB and push it
    /// to the connected node (if any). Safe to call after CRUD.
    pub async fn push_config(&self, db: &PgPool, node_id: &str) {
        let snapshot = match build_config_snapshot(db, node_id).await {
            Ok(s) => s,
            Err(e) => {
                tracing::error!(%node_id, error = %e, "failed to build config snapshot");
                return;
            }
        };
        let g = self.inner.read().await;
        if let Some(conn) = g.get(node_id) {
            // watch::Sender::send replaces previous value (latest-wins).
            let _ = conn.config_tx.send(Some(snapshot));
        }
    }

    /// Forcefully drop the active stream for `node_id`, if any. Used after
    /// node deletion or cert rotation: removing the entry drops the
    /// `watch::Sender`, which surfaces as a `RecvError` in the outbound
    /// forwarder, which closes the gRPC stream.
    pub async fn force_kick(&self, node_id: &str) {
        let mut g = self.inner.write().await;
        if g.remove(node_id).is_some() {
            tracing::info!(%node_id, "force-kicked active channel");
        }
    }

    /// Send a Probe to the connected node and await its result with a
    /// timeout. Returns `Err` if the node isn't connected, the channel is
    /// closed, or the timeout elapses.
    #[allow(dead_code)]
    pub async fn probe(
        &self,
        node_id: &str,
        target: String,
        timeout: Duration,
    ) -> Result<ProbeResult, ProbeError> {
        self.probe_with_kind(
            node_id,
            target,
            timeout,
            relay_proto::v1::ProbeKind::Connect,
        )
        .await
    }

    /// Same as `probe` but lets the caller pick the probe kind (connect /
    /// bind-tcp / bind-udp).
    pub async fn probe_with_kind(
        &self,
        node_id: &str,
        target: String,
        timeout: Duration,
        kind: relay_proto::v1::ProbeKind,
    ) -> Result<ProbeResult, ProbeError> {
        let request_id = PROBE_SEQ.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        {
            let mut p = self.pending_probes.lock().await;
            p.insert(request_id, tx);
        }

        let timeout_ms: u32 = timeout.as_millis().min(u32::MAX as u128) as u32;
        let msg = MasterMessage {
            payload: Some(MasterPayload::Probe(ProbeRequest {
                request_id,
                target,
                timeout_ms,
                kind: kind as i32,
            })),
        };

        let send_result = {
            let g = self.inner.read().await;
            match g.get(node_id) {
                Some(conn) => conn.out_tx.send(Ok(msg)).await,
                None => {
                    self.pending_probes.lock().await.remove(&request_id);
                    return Err(ProbeError::NodeOffline);
                }
            }
        };
        if send_result.is_err() {
            self.pending_probes.lock().await.remove(&request_id);
            return Err(ProbeError::NodeOffline);
        }

        match tokio::time::timeout(timeout + Duration::from_secs(2), rx).await {
            Ok(Ok(result)) => Ok(result),
            Ok(Err(_)) => Err(ProbeError::NodeOffline),
            Err(_) => {
                self.pending_probes.lock().await.remove(&request_id);
                Err(ProbeError::Timeout)
            }
        }
    }

    /// Send a `Command` message to a connected node. Errors if the node is
    /// not currently connected. Best-effort: a successful send only means the
    /// outbound channel accepted the message, not that the node received or
    /// acted on it.
    pub async fn send_command(
        &self,
        node_id: &str,
        cmd: relay_proto::v1::Command,
    ) -> Result<(), ProbeError> {
        let g = self.inner.read().await;
        let conn = g.get(node_id).ok_or(ProbeError::NodeOffline)?;
        let msg = MasterMessage {
            payload: Some(MasterPayload::Command(cmd)),
        };
        conn.out_tx
            .send(Ok(msg))
            .await
            .map_err(|_| ProbeError::NodeOffline)
    }

    /// Called by the gRPC inbound handler when a ProbeResult arrives;
    /// delivers it to the awaiting `probe()` caller.
    pub async fn deliver_probe_result(&self, result: ProbeResult) {
        let mut p = self.pending_probes.lock().await;
        if let Some(tx) = p.remove(&result.request_id) {
            let _ = tx.send(result);
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ProbeError {
    #[error("node is not connected")]
    NodeOffline,
    #[error("probe timed out")]
    Timeout,
}

pub async fn build_config_snapshot(db: &PgPool, node_id: &str) -> sqlx::Result<ConfigUpdate> {
    use relay_proto::v1::Protocol;

    let (listen_stack,): (String,) = sqlx::query_as("SELECT listen_stack FROM nodes WHERE id = $1")
        .bind(node_id)
        .fetch_one(db)
        .await?;

    // Layered DAG: 单一跳可能由多个节点组成（DNS-LB / fan-in），同层共享
    // listen_port（由 0007 的 deferrable trigger 保证）。两阶段读取：
    //
    //   Stage 1 (`hop_rows`)：当前节点承载的每一行 forward_ports，附带 forward
    //   级静态字段、effective_enabled 计算、speed_limit。**不再** join prev/next
    //   节点 —— 单 join 在多节点层会笛卡尔展开行数。
    //
    //   Stage 2 (`layer_rows`)：所有相关 forward 的所有层（含本节点不直接承载
    //   的层），每个 (forward_id, hop_index) 拿到该层 listen_port 与该层每个
    //   节点的首个非空 server_ip。在 Rust 端按 (fid, hop) 聚合得到：
    //     - prev 层 IPs → 中转层 ACL
    //     - next 层 IPs × next listen_port → upstream_addrs（多节点 = 多 upstream）
    type HopRow = (
        i64,         // forward_id
        i32,         // hop_index
        i32,         // last_hop_index
        i32,         // listen_port (this hop)
        String,      // protocol
        Vec<String>, // forwards.remote_addrs (final targets)
        String,      // lb_strategy
        i32,         // max_connections
        Vec<String>, // user allow_cidrs (entry only)
        Vec<String>, // user deny_cidrs (entry only)
        bool,        // effective_enabled
        i64,         // deploy_generation
        i64,         // speed_limit_kbps
    );
    let hop_rows: Vec<HopRow> = sqlx::query_as(
        "SELECT f.id, fp.hop_index,
                (SELECT MAX(hop_index) FROM forward_ports WHERE forward_id = f.id) AS last_idx,
                fp.listen_port, fp.protocol, f.remote_addrs, f.lb_strategy,
                f.max_connections, f.allow_cidrs, f.deny_cidrs,
                (
                  f.desired_enabled
                  AND t.enabled
                  AND ut.enabled
                  AND (ut.expires_at IS NULL OR ut.expires_at > now())
                  AND u.status = 'active'
                  AND (u.expires_at IS NULL OR u.expires_at > now())
                  AND NOT EXISTS (
                    SELECT 1 FROM forward_pause_reasons pr WHERE pr.forward_id = f.id
                  )
                ) AS effective_enabled,
                f.deploy_generation,
                ut.speed_limit_kbps
           FROM forward_ports fp
           JOIN forwards f       ON f.id = fp.forward_id
           JOIN user_tunnels ut  ON ut.id = f.user_tunnel_id
           JOIN tunnels t        ON t.id  = ut.tunnel_id
           JOIN users u          ON u.id  = ut.user_id
          WHERE fp.node_id = $1
          ORDER BY f.id, fp.hop_index, fp.protocol",
    )
    .bind(node_id)
    .fetch_all(db)
    .await?;

    // Stage 2: 拉取所有相关 forward 的所有层信息。DISTINCT 是因为 forward_ports
    // 一行/protocol，而 listen_port 跨 protocol 一致，节点列表也跨 protocol 一致。
    type LayerRow = (i64, i32, i32, String, Vec<String>);
    let layer_rows: Vec<LayerRow> = sqlx::query_as(
        "SELECT DISTINCT fp.forward_id, fp.hop_index, fp.listen_port, fp.node_id, n.server_ips
           FROM forward_ports fp
           JOIN nodes n ON n.id = fp.node_id
          WHERE fp.forward_id IN (
              SELECT DISTINCT forward_id FROM forward_ports WHERE node_id = $1
          )",
    )
    .bind(node_id)
    .fetch_all(db)
    .await?;

    struct LayerInfo {
        listen_port: i32,
        first_ips: Vec<String>,
    }
    let mut layers: HashMap<(i64, i32), LayerInfo> = HashMap::new();
    for (fid, hop, port, _nid, server_ips) in layer_rows {
        let entry = layers.entry((fid, hop)).or_insert(LayerInfo {
            listen_port: port,
            first_ips: Vec::new(),
        });
        if let Some(first) = server_ips.into_iter().find(|s| !s.trim().is_empty()) {
            if !entry.first_ips.contains(&first) {
                entry.first_ips.push(first);
            }
        }
    }

    let forwards: Vec<ForwardConfig> = hop_rows
        .into_iter()
        .map(
            |(
                fid,
                hop_index,
                last_idx,
                listen_port,
                proto,
                final_upstreams,
                lb_strategy,
                maxc,
                user_allow,
                user_deny,
                effective_enabled,
                deploy_generation,
                speed_limit_kbps,
            )| {
                let upstream_addrs = if hop_index == last_idx {
                    final_upstreams
                } else if let Some(next) = layers.get(&(fid, hop_index + 1)) {
                    let mut addrs: Vec<String> = next
                        .first_ips
                        .iter()
                        .map(|ip| format!("{ip}:{}", next.listen_port))
                        .collect();
                    if addrs.is_empty() {
                        addrs.push("0.0.0.0:0".to_string());
                    }
                    addrs
                } else {
                    vec!["0.0.0.0:0".to_string()]
                };

                let (allow_cidrs, deny_cidrs) = if hop_index == 0 {
                    (user_allow, user_deny)
                } else {
                    (Vec::new(), Vec::new())
                };

                ForwardConfig {
                    forward_id: fid.to_string(),
                    hop_index: hop_index as u32,
                    protocol: match proto.as_str() {
                        "udp" => Protocol::Udp as i32,
                        _ => Protocol::Tcp as i32,
                    },
                    listen_addr: if listen_stack == "v4" {
                        format!("0.0.0.0:{listen_port}")
                    } else {
                        format!("[::]:{listen_port}")
                    },
                    v6_only: listen_stack == "v6",
                    upstream_addrs,
                    lb_strategy,
                    max_connections: maxc as u32,
                    allow_cidrs,
                    deny_cidrs,
                    enabled: effective_enabled,
                    deploy_generation: deploy_generation as u64,
                    speed_limit_kbps: if hop_index == 0 {
                        speed_limit_kbps as u64
                    } else {
                        0
                    },
                }
            },
        )
        .collect();

    // 用所有 ForwardConfig 字段的内容算指纹，替代单调自增版本号
    let version: u64 = {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        for f in &forwards {
            h.update(f.forward_id.as_bytes());
            h.update(f.hop_index.to_le_bytes());
            h.update((f.protocol as u32).to_le_bytes());
            h.update(f.listen_addr.as_bytes());
            for addr in &f.upstream_addrs {
                h.update(addr.as_bytes());
            }
            h.update(f.max_connections.to_le_bytes());
            for cidr in &f.allow_cidrs {
                h.update(cidr.as_bytes());
            }
            for cidr in &f.deny_cidrs {
                h.update(cidr.as_bytes());
            }
            h.update([f.enabled as u8]);
            h.update(f.lb_strategy.as_bytes());
            h.update(f.deploy_generation.to_le_bytes());
            h.update(f.speed_limit_kbps.to_le_bytes());
        }
        u64::from_le_bytes(h.finalize()[..8].try_into().unwrap())
    };

    let _ = sqlx::query("UPDATE nodes SET tunnels_version = $2 WHERE id = $1")
        .bind(node_id)
        .bind(version as i64)
        .execute(db)
        .await;

    Ok(ConfigUpdate { version, forwards })
}

/// Convert a node address (host or IP, no port) into a CIDR usable in
/// allow_cidrs. IPv4/IPv6 literals get /32 or /128. For DNS hostnames we
/// fail closed by returning an unreachable sentinel: an empty allow list on
/// the node side would mean "accept from anywhere", silently exposing the
/// intermediate listen port to the public internet — a serious bypass.
pub fn config_to_master_msg(cfg: ConfigUpdate) -> MasterMessage {
    MasterMessage {
        payload: Some(MasterPayload::Config(cfg)),
    }
}

//! Master gRPC server (mTLS, M4.3).
//!
//! Authentication is entirely transport-layer:
//!   1. TLS handshake requires a client cert signed by the master CA.
//!   2. The Channel handler extracts `node_id` from the leaf cert's
//!      `subjectAlternativeName` (DNS) and verifies the cert's SHA-256
//!      fingerprint matches `nodes.cert_fingerprint` in the DB.
//!
//! There is no longer a `Register` RPC or app-layer session token. Bootstrap
//! happens out-of-band via `Enroll` on the dedicated TLS listener (see
//! `enroll.rs` / M4.2).

use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use futures_core::Stream;
use sha2::{Digest, Sha256};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tokio_stream::StreamExt;
use tonic::transport::server::{TcpConnectInfo, TlsConnectInfo};
use tonic::transport::{Certificate, Identity, Server, ServerTlsConfig};
use tonic::{Request, Response, Status, Streaming};
use x509_parser::prelude::*;

use relay_proto::v1::{
    master_message::Payload as MasterPayload,
    node_message::Payload as NodePayload,
    node_service_server::{NodeService, NodeServiceServer},
    ConfigAck, MasterMessage, NodeMessage, RenewCertResponse,
};

use crate::pki::Pki;
use crate::registry::config_to_master_msg;
use crate::state::AppState;

pub async fn serve(addr: SocketAddr, state: AppState, pki: Arc<Pki>) -> anyhow::Result<()> {
    let identity = Identity::from_pem(&pki.server_cert_pem, &pki.server_key_pem);
    let ca = Certificate::from_pem(pki.ca_cert_pem.as_bytes());
    let tls = ServerTlsConfig::new().identity(identity).client_ca_root(ca);

    let svc = NodeServiceImpl { state };
    Server::builder()
        .tls_config(tls)?
        .http2_keepalive_interval(Some(Duration::from_secs(20)))
        .http2_keepalive_timeout(Some(Duration::from_secs(10)))
        .add_service(NodeServiceServer::new(svc))
        .serve(addr)
        .await?;
    Ok(())
}

#[derive(Clone)]
struct NodeServiceImpl {
    state: AppState,
}

/// Pull the leaf client cert out of a tonic Request's TLS extensions, then
/// extract the node_id (first DNS SAN) and SHA-256 fingerprint.
#[allow(clippy::result_large_err)]
fn peer_node_identity<T>(req: &Request<T>) -> Result<(String, String), Status> {
    let info = req
        .extensions()
        .get::<TlsConnectInfo<TcpConnectInfo>>()
        .ok_or_else(|| Status::unauthenticated("TLS connection info missing"))?;
    let certs = info
        .peer_certs()
        .ok_or_else(|| Status::unauthenticated("client did not present a cert"))?;
    let leaf = certs
        .first()
        .ok_or_else(|| Status::unauthenticated("empty client cert chain"))?;

    let (_, cert) = X509Certificate::from_der(leaf.as_ref())
        .map_err(|e| Status::unauthenticated(format!("client cert parse: {e}")))?;

    let san = cert
        .tbs_certificate
        .subject_alternative_name()
        .map_err(|e| Status::unauthenticated(format!("SAN parse: {e}")))?
        .ok_or_else(|| Status::unauthenticated("client cert has no SAN"))?;

    let node_id = san
        .value
        .general_names
        .iter()
        .find_map(|gn| match gn {
            GeneralName::DNSName(s) => Some((*s).to_string()),
            GeneralName::IPAddress(bytes) => match <[u8; 4]>::try_from(*bytes) {
                Ok(arr) => Some(std::net::Ipv4Addr::from(arr).to_string()),
                Err(_) => <[u8; 16]>::try_from(*bytes)
                    .ok()
                    .map(|arr| std::net::Ipv6Addr::from(arr).to_string()),
            },
            _ => None,
        })
        .ok_or_else(|| Status::unauthenticated("client cert SAN has no DNS or IP entry"))?;

    let fingerprint = hex_lower(&Sha256::digest(leaf.as_ref()));
    Ok((node_id, fingerprint))
}

fn hex_lower(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{:02x}", b));
    }
    s
}

#[tonic::async_trait]
impl NodeService for NodeServiceImpl {
    type ChannelStream =
        Pin<Box<dyn Stream<Item = Result<MasterMessage, Status>> + Send + 'static>>;

    async fn channel(
        &self,
        request: Request<Streaming<NodeMessage>>,
    ) -> Result<Response<Self::ChannelStream>, Status> {
        let (node_id, fingerprint) = peer_node_identity(&request)?;

        // Verify the cert presented matches the one we last issued for this
        // node. Mismatch → either (a) cert was rotated/revoked or (b) row
        // has no cert yet (never enrolled). Either way, refuse.
        let row: Option<(Option<String>,)> =
            sqlx::query_as("SELECT cert_fingerprint FROM nodes WHERE id = $1")
                .bind(&node_id)
                .fetch_optional(&self.state.db)
                .await
                .map_err(internal)?;
        let stored = row
            .ok_or_else(|| Status::unauthenticated("node not registered"))?
            .0
            .ok_or_else(|| Status::unauthenticated("node not enrolled"))?;
        if stored != fingerprint {
            tracing::warn!(
                %node_id,
                presented = %fingerprint,
                stored = %stored,
                "client cert fingerprint mismatch"
            );
            return Err(Status::unauthenticated(
                "client cert fingerprint does not match issued cert",
            ));
        }

        // Refresh enrollment / liveness timestamps on every (re)connect, and
        // auto-populate server_ips with the peer IP if it's still empty
        // (first-connect bootstrap; never overwrites user-managed values).
        let remote_ip = request
            .extensions()
            .get::<TlsConnectInfo<TcpConnectInfo>>()
            .and_then(|info| info.get_ref().remote_addr())
            .map(|sa| sa.ip().to_string());
        let auto_fill_row: Result<Option<(Vec<String>,)>, _> = sqlx::query_as(
            "UPDATE nodes
                SET enrolled_at = COALESCE(enrolled_at, now()),
                    last_seen_at = now(),
                    updated_at = now(),
                    server_ips = CASE
                      WHEN cardinality(server_ips) = 0 AND $2::TEXT IS NOT NULL
                      THEN ARRAY[$2::TEXT]
                      ELSE server_ips
                    END
              WHERE id = $1
              RETURNING server_ips",
        )
        .bind(&node_id)
        .bind(&remote_ip)
        .fetch_optional(&self.state.db)
        .await;
        if let Ok(Some((ips,))) = &auto_fill_row {
            if let Some(ip) = &remote_ip {
                if ips.len() == 1 && ips.first().map(String::as_str) == Some(ip.as_str()) {
                    tracing::info!(
                        %node_id, peer_ip = %ip,
                        "auto-populated server_ips from peer_addr"
                    );
                }
            }
        }

        let inbound = request.into_inner();
        let (out_tx, out_rx) = mpsc::channel::<Result<MasterMessage, Status>>(32);

        // Subscribe — displaces any prior connection for this node.
        let sub = self
            .state
            .registry
            .subscribe(node_id.clone(), out_tx.clone())
            .await;
        let conn_id = sub.conn_id;
        tracing::info!(%node_id, conn_id, "channel opened (mTLS)");

        // Push initial config snapshot into the watch (latest-wins).
        self.state
            .registry
            .push_config(&self.state.db, &node_id)
            .await;

        let state_for_in = self.state.clone();
        let pki = self.state.pki.clone();
        let node_id_for_in = node_id.clone();
        let out_tx_for_in = out_tx.clone();
        tokio::spawn(async move {
            handle_inbound(state_for_in, pki, node_id_for_in, inbound, out_tx_for_in).await;
        });

        // Forward watch updates -> outbound mpsc.
        let mut sub = sub; // own
        tokio::spawn(async move {
            // Drain any value already present (initial snapshot).
            {
                let snap = sub.config_rx.borrow_and_update().clone();
                if let Some(cfg) = snap {
                    if out_tx.send(Ok(config_to_master_msg(cfg))).await.is_err() {
                        return;
                    }
                }
            }
            while sub.config_rx.changed().await.is_ok() {
                let snap = sub.config_rx.borrow_and_update().clone();
                if let Some(cfg) = snap {
                    if out_tx.send(Ok(config_to_master_msg(cfg))).await.is_err() {
                        break;
                    }
                }
            }
            // sub is dropped here -> registry entry removed (if still ours).
            drop(sub);
        });

        let stream = ReceiverStream::new(out_rx);
        Ok(Response::new(Box::pin(stream) as Self::ChannelStream))
    }
}

#[allow(clippy::too_many_arguments)]
async fn handle_inbound(
    state: AppState,
    pki: Arc<Pki>,
    node_id: String,
    mut inbound: Streaming<NodeMessage>,
    out_tx: mpsc::Sender<Result<MasterMessage, Status>>,
) {
    let db = state.db.clone();
    let series = state.series.clone();
    let counter_deltas = state.counter_deltas.clone();
    let registry = state.registry.clone();
    let stats_tx = state.stats_tx.clone();
    let speed_tx = state.speed_tx.clone();

    // 连接建立时一次性加载，5s 的陈旧度可接受
    let node_traffic_ratio: f64 =
        sqlx::query_scalar("SELECT traffic_ratio FROM nodes WHERE id = $1")
            .bind(&node_id)
            .fetch_optional(&db)
            .await
            .ok()
            .flatten()
            .unwrap_or_else(|| {
                tracing::warn!(node_id = %node_id, "traffic_ratio not found, defaulting to 1.0");
                1.0
            });

    while let Some(item) = inbound.next().await {
        let Ok(msg) = item else { break };
        let Some(payload) = msg.payload else { continue };
        match payload {
            NodePayload::Heartbeat(hb) => {
                if hb.protocol_version != 0 && hb.protocol_version < relay_common::PROTOCOL_VERSION
                {
                    tracing::warn!(
                        node_id = %node_id,
                        agent_version = %hb.agent_version,
                        node_protocol = hb.protocol_version,
                        master_protocol = relay_common::PROTOCOL_VERSION,
                        "node protocol version is older than master; node should be upgraded"
                    );
                }
                let json = serde_json::json!({
                    "ts_unix_ms": hb.ts_unix_ms,
                    "cpu_pct": hb.cpu_pct,
                    "mem_used_bytes": hb.mem_used_bytes,
                    "mem_total_bytes": hb.mem_total_bytes,
                    "active_connections": hb.active_connections,
                });
                let now = Utc::now();
                let proto_ver = hb.protocol_version as i32;
                let caps = hb.capabilities.clone();

                // 1) 更新 in-process L1（永远最新，单 master 真相源）
                let needs_pg_write = {
                    use std::collections::hash_map::Entry;
                    let mut map = state.node_runtime.write().await;
                    match map.entry(node_id.clone()) {
                        Entry::Occupied(mut o) => {
                            let e = o.get_mut();
                            let throttle = (now - e.last_pg_write_at).num_seconds() >= 30;
                            e.last_heartbeat = json.clone();
                            e.last_seen_at = now;
                            if !hb.agent_version.is_empty() {
                                e.version = hb.agent_version.clone();
                            }
                            e.protocol_version = proto_ver;
                            e.capabilities = caps.clone();
                            throttle
                        }
                        Entry::Vacant(v) => {
                            v.insert(crate::state::NodeRuntimeEntry {
                                last_heartbeat: json.clone(),
                                last_seen_at: now,
                                version: hb.agent_version.clone(),
                                protocol_version: proto_ver,
                                capabilities: caps.clone(),
                                last_pg_write_at: chrono::DateTime::<Utc>::MIN_UTC,
                            });
                            true
                        }
                    }
                };

                // 2) 镜像写 Redis L2（best-effort，只为重启 warmup 提供数据）
                let payload = crate::cache::node::RuntimePayload {
                    last_heartbeat: json.clone(),
                    last_seen_at: now,
                    version: hb.agent_version.clone(),
                    protocol_version: proto_ver,
                    capabilities: caps,
                };
                crate::cache::node::write(&state.redis, &node_id, &payload).await;

                // 3) 节流写 PG（每节点至多每 30s 一次；timestamp-guarded 防 reconnect 路径回退）
                if needs_pg_write {
                    let res = sqlx::query(
                        "UPDATE nodes
                            SET last_heartbeat = $2,
                                last_seen_at = $3,
                                version = COALESCE(NULLIF($4, ''), version),
                                protocol_version = $5
                          WHERE id = $1
                            AND (last_seen_at IS NULL OR last_seen_at <= $3)",
                    )
                    .bind(&node_id)
                    .bind(json)
                    .bind(now)
                    .bind(&hb.agent_version)
                    .bind(proto_ver)
                    .execute(&db)
                    .await;
                    if res.is_ok() {
                        if let Some(e) = state.node_runtime.write().await.get_mut(&node_id) {
                            e.last_pg_write_at = now;
                        }
                    }
                }

                // 4) node_availability 每节点每分钟去重
                {
                    let minute = now.timestamp() / 60;
                    let key = (node_id.clone(), minute);
                    let mut seen = state.availability_seen.lock().await;
                    if seen.insert(key) {
                        let _ = sqlx::query(
                            "INSERT INTO node_availability (node_id, recorded_at)
                             VALUES ($1, date_trunc('minute', now()))
                             ON CONFLICT DO NOTHING",
                        )
                        .bind(&node_id)
                        .execute(&db)
                        .await;
                        // 修剪超过 3 分钟的旧条目，避免无界增长
                        seen.retain(|(_, m)| *m >= minute - 3);
                    }
                }

                series
                    .push_heartbeat(
                        &node_id,
                        crate::series::HeartbeatSample {
                            ts_unix_ms: hb.ts_unix_ms,
                            cpu_pct: hb.cpu_pct,
                            mem_used_bytes: hb.mem_used_bytes,
                            mem_total_bytes: hb.mem_total_bytes,
                            active_connections: hb.active_connections,
                        },
                    )
                    .await;

                // 持久化心跳到时序表（fire-and-forget，不阻塞心跳流）
                {
                    let db2 = db.clone();
                    let nid = node_id.clone();
                    let cpu = hb.cpu_pct;
                    let mem_used = hb.mem_used_bytes as i64;
                    let mem_total = hb.mem_total_bytes as i64;
                    let conns = hb.active_connections as i32;
                    tokio::spawn(async move {
                        let _ = sqlx::query(
                            "INSERT INTO node_heartbeats \
                             (ts, node_id, cpu_pct, mem_used_bytes, mem_total_bytes, active_connections) \
                             VALUES (now(), $1, $2, $3, $4, $5)",
                        )
                        .bind(nid)
                        .bind(cpu)
                        .bind(mem_used)
                        .bind(mem_total)
                        .bind(conns)
                        .execute(&db2)
                        .await;
                    });
                }

                // 5) 升级 job 完成判定：心跳的 agent_version == in-flight job
                //    的 target_tag（去掉 v 前缀后比较）→ 标 succeeded。
                if !hb.agent_version.is_empty() {
                    let cur = crate::upgrade::normalize_version(&hb.agent_version);
                    let _ = sqlx::query(
                        "UPDATE upgrade_jobs
                            SET state = 'succeeded',
                                completed_at = now()
                          WHERE node_id = $1
                            AND state IN ('dispatched','accepted')
                            AND regexp_replace(target_tag, '^v', '') = $2",
                    )
                    .bind(&node_id)
                    .bind(&cur)
                    .execute(&db)
                    .await;
                }
            }
            NodePayload::Stats(st) => {
                let now_ms = Utc::now().timestamp_millis();
                let stats_key = format!("{}:{}", st.forward_id, st.hop_index);
                series
                    .push_tunnel(
                        &node_id,
                        &stats_key,
                        crate::series::TunnelSample {
                            ts_unix_ms: now_ms,
                            bytes_in: st.bytes_in,
                            bytes_out: st.bytes_out,
                            active_connections: st.active_connections,
                            total_connections: st.total_connections,
                        },
                    )
                    .await;
                let _ = stats_tx.send(crate::state::ForwardStatEvent {
                    forward_id: st.forward_id.clone(),
                    node_id: node_id.clone(),
                    hop_index: st.hop_index,
                    bytes_in: st.bytes_in,
                    bytes_out: st.bytes_out,
                    active_connections: st.active_connections,
                    ts_unix_ms: now_ms,
                });
                // 入口跳快照：更新活跃客户端 IP 内存存储
                if st.hop_index == 0 && !st.active_client_ips.is_empty() {
                    state
                        .active_conns
                        .update(&st.forward_id, st.active_client_ips.clone())
                        .await;
                } else if st.hop_index == 0 && st.active_connections == 0 {
                    state.active_conns.update(&st.forward_id, vec![]).await;
                }
                let (rx, tx) = series.node_speed(&node_id).await;
                speed_tx.send_modify(|m| {
                    m.insert(
                        node_id.clone(),
                        crate::state::NodeSpeed {
                            rx_bps: rx,
                            tx_bps: tx,
                        },
                    );
                });
                // Each hop accumulates its own bytes weighted by the node's
                // traffic_ratio, so multi-hop billing sums A*ratio_A + B*ratio_B.
                if let Ok(forward_id) = st.forward_id.parse::<i64>() {
                    let (d_in, d_out) = counter_deltas
                        .record(
                            &node_id,
                            forward_id,
                            st.hop_index,
                            st.bytes_in,
                            st.bytes_out,
                        )
                        .await;
                    if d_in > 0 || d_out > 0 {
                        let billed_in = (d_in as f64 * node_traffic_ratio).round() as i64;
                        let billed_out = (d_out as f64 * node_traffic_ratio).round() as i64;
                        if billed_in > 0 || billed_out > 0 {
                            let _ = sqlx::query(
                                "UPDATE forwards
                                    SET in_flow_bytes  = in_flow_bytes  + $2,
                                        out_flow_bytes = out_flow_bytes + $3
                                  WHERE id = $1",
                            )
                            .bind(forward_id)
                            .bind(billed_in)
                            .bind(billed_out)
                            .execute(&db)
                            .await;

                            // Quota enforcement: write tunnel_quota_exceeded
                            // reason to every forward in this user_tunnel
                            // when the aggregate (in+out) crosses limit.
                            let quota: Option<(i64, i64, i64)> = sqlx::query_as(
                                "SELECT ut.id, ut.flow_limit_bytes,
                                        COALESCE(SUM(f.in_flow_bytes + f.out_flow_bytes),0)::BIGINT
                                   FROM forwards f0
                                   JOIN user_tunnels ut ON ut.id = f0.user_tunnel_id
                                   JOIN forwards f ON f.user_tunnel_id = ut.id
                                  WHERE f0.id = $1
                                  GROUP BY ut.id, ut.flow_limit_bytes",
                            )
                            .bind(forward_id)
                            .fetch_optional(&db)
                            .await
                            .ok()
                            .flatten();
                            if let Some((ut_id, limit, used)) = quota {
                                if limit > 0 && used >= limit {
                                    let affected: Vec<(i64,)> = sqlx::query_as(
                                        "SELECT id FROM forwards WHERE user_tunnel_id = $1",
                                    )
                                    .bind(ut_id)
                                    .fetch_all(&db)
                                    .await
                                    .unwrap_or_default();
                                    let mut bumped = false;
                                    for (fid,) in &affected {
                                        if let Ok(true) = crate::pause::write_pause_reason(
                                            &db,
                                            *fid,
                                            crate::pause::REASON_TUNNEL_QUOTA_EXCEEDED,
                                        )
                                        .await
                                        {
                                            bumped = true;
                                        }
                                    }
                                    if bumped {
                                        // Bump every node holding a hop for
                                        // these forwards and push.
                                        let nodes: Vec<(String,)> = sqlx::query_as(
                                            "SELECT DISTINCT node_id FROM forward_ports
                                               WHERE forward_id = ANY($1)",
                                        )
                                        .bind(affected.iter().map(|(f,)| *f).collect::<Vec<_>>())
                                        .fetch_all(&db)
                                        .await
                                        .unwrap_or_default();
                                        for (nid,) in &nodes {
                                            registry.push_config(&db, nid).await;
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            NodePayload::Event(ev) => {
                tracing::info!(%node_id, code = %ev.code, level = ev.level,
                    msg = %ev.message, "node event");
            }
            NodePayload::Ack(ConfigAck {
                config_version,
                success,
                error,
            }) => {
                if success {
                    let _ = sqlx::query("UPDATE nodes SET last_applied_version = $2 WHERE id = $1")
                        .bind(&node_id)
                        .bind(config_version as i64)
                        .execute(&db)
                        .await;
                    let _ = sqlx::query(
                        "UPDATE forward_ports SET last_applied_at = now() WHERE node_id = $1",
                    )
                    .bind(&node_id)
                    .execute(&db)
                    .await;
                    tracing::info!(%node_id, version = config_version, "config applied");
                } else {
                    tracing::warn!(%node_id, version = config_version, %error, "config apply failed");
                }
            }
            NodePayload::ProbeResult(pr) => {
                registry.deliver_probe_result(pr).await;
            }
            NodePayload::RenewCert(req) => {
                let resp = match pki.sign_node_cert(&node_id, &req.csr_pem) {
                    Ok(signed) => {
                        let not_after_chrono = chrono::DateTime::<Utc>::from_timestamp(
                            signed.not_after.unix_timestamp(),
                            0,
                        );
                        let db_res = sqlx::query(
                            "UPDATE nodes
                                SET cert_serial = $2,
                                    cert_fingerprint = $3,
                                    cert_not_after = $4,
                                    updated_at = now()
                              WHERE id = $1",
                        )
                        .bind(&node_id)
                        .bind(&signed.serial_hex)
                        .bind(&signed.fingerprint_hex)
                        .bind(not_after_chrono)
                        .execute(&db)
                        .await;
                        if let Err(e) = db_res {
                            tracing::error!(%node_id, error = %e, "renew_cert: db update failed");
                            RenewCertResponse {
                                node_cert_pem: String::new(),
                                not_after_unix_ms: 0,
                                error: format!("db: {e}"),
                            }
                        } else {
                            tracing::info!(
                                %node_id,
                                fingerprint = %signed.fingerprint_hex,
                                "cert renewed"
                            );
                            RenewCertResponse {
                                node_cert_pem: signed.cert_pem,
                                not_after_unix_ms: signed.not_after.unix_timestamp() * 1000,
                                error: String::new(),
                            }
                        }
                    }
                    Err(e) => {
                        tracing::warn!(%node_id, error = %e, "renew_cert: sign failed");
                        RenewCertResponse {
                            node_cert_pem: String::new(),
                            not_after_unix_ms: 0,
                            error: e.to_string(),
                        }
                    }
                };
                if let Err(e) = out_tx
                    .send(Ok(MasterMessage {
                        payload: Some(MasterPayload::RenewCert(resp)),
                    }))
                    .await
                {
                    tracing::warn!(%node_id, error = %e, "renew_cert: outbound channel closed");
                }
            }
            NodePayload::UpstreamProbe(sample) => {
                // fire-and-forget，忽略写 DB 错误（不影响控制流）
                let db = db.clone();
                let registry = registry.clone();
                tokio::spawn(async move {
                    let success = sample.latency_us > 0;
                    let latency: Option<i64> = if success {
                        Some(sample.latency_us as i64)
                    } else {
                        None
                    };
                    if let Ok(fid) = sample.forward_id.parse::<i64>() {
                        let _ = sqlx::query(
                            "INSERT INTO forward_probe_samples \
                             (forward_id, upstream_addr, latency_us, probed_at) \
                             VALUES ($1, $2, $3, to_timestamp($4::double precision / 1000))",
                        )
                        .bind(fid)
                        .bind(&sample.upstream_addr)
                        .bind(latency)
                        .bind(sample.ts_unix_ms)
                        .execute(&db)
                        .await;

                        // 健康状态跟踪：状态变化时推送新配置
                        if crate::health_checker::process_probe(
                            &db,
                            fid,
                            &sample.upstream_addr,
                            success,
                        )
                        .await
                        {
                            crate::health_checker::push_for_forward(&db, &registry, fid).await;
                        }
                    }
                });
            }
            NodePayload::ClientConnect(ev) => {
                if let Ok(fwd_id) = ev.forward_id.parse::<i64>() {
                    let db = db.clone();
                    let node_id = node_id.clone();
                    let log_id = crate::snowflake::next_id();
                    let connected_at =
                        chrono::DateTime::<Utc>::from_timestamp_millis(ev.ts_unix_ms)
                            .unwrap_or_else(Utc::now);
                    tokio::spawn(async move {
                        let _ = sqlx::query(
                            "INSERT INTO connection_logs \
                             (id, forward_id, node_id, conn_id, client_ip, connected_at) \
                             VALUES ($1, $2, $3, $4, $5, $6) \
                             ON CONFLICT (conn_id) DO NOTHING",
                        )
                        .bind(log_id)
                        .bind(fwd_id)
                        .bind(&node_id)
                        .bind(&ev.conn_id)
                        .bind(&ev.client_ip)
                        .bind(connected_at)
                        .execute(&db)
                        .await;
                    });
                }
            }
            NodePayload::ClientDisconnect(ev) => {
                let db = db.clone();
                let disconnected_at = chrono::DateTime::<Utc>::from_timestamp_millis(ev.ts_unix_ms)
                    .unwrap_or_else(Utc::now);
                tokio::spawn(async move {
                    let _ = sqlx::query(
                        "UPDATE connection_logs \
                            SET disconnected_at = $2, \
                                bytes_in        = $3, \
                                bytes_out       = $4 \
                          WHERE conn_id = $1 \
                            AND disconnected_at IS NULL",
                    )
                    .bind(&ev.conn_id)
                    .bind(disconnected_at)
                    .bind(ev.bytes_in as i64)
                    .bind(ev.bytes_out as i64)
                    .execute(&db)
                    .await;
                });
            }
            NodePayload::UpgradeReport(rep) => {
                tracing::info!(
                    %node_id,
                    job_id = rep.job_id,
                    state = %rep.state,
                    error = %rep.error,
                    "upgrade report"
                );
                match rep.state.as_str() {
                    "accepted" => {
                        let res = sqlx::query(
                            "UPDATE upgrade_jobs
                                SET state = 'accepted',
                                    accepted_at = now()
                              WHERE id = $1
                                AND node_id = $2
                                AND state IN ('queued','dispatched')",
                        )
                        .bind(rep.job_id)
                        .bind(&node_id)
                        .execute(&db)
                        .await;
                        match res {
                            Ok(r) if r.rows_affected() == 0 => tracing::warn!(
                                %node_id,
                                job_id = rep.job_id,
                                "upgrade report 'accepted' did not match any in-flight job for this node"
                            ),
                            Ok(_) => {}
                            Err(e) => {
                                tracing::warn!(%node_id, error = %e, "upgrade report db update failed")
                            }
                        }
                    }
                    "failed" => {
                        let res = sqlx::query(
                            "UPDATE upgrade_jobs
                                SET state = 'failed',
                                    error = $3,
                                    completed_at = now()
                              WHERE id = $1
                                AND node_id = $2
                                AND state IN ('queued','dispatched','accepted')",
                        )
                        .bind(rep.job_id)
                        .bind(&node_id)
                        .bind(if rep.error.is_empty() {
                            "node rejected upgrade".to_string()
                        } else {
                            rep.error.clone()
                        })
                        .execute(&db)
                        .await;
                        match res {
                            Ok(r) if r.rows_affected() == 0 => tracing::warn!(
                                %node_id,
                                job_id = rep.job_id,
                                "upgrade report 'failed' did not match any in-flight job for this node"
                            ),
                            Ok(_) => {}
                            Err(e) => {
                                tracing::warn!(%node_id, error = %e, "upgrade report db update failed")
                            }
                        }
                    }
                    other => {
                        tracing::warn!(%node_id, state = other, "unknown upgrade report state");
                    }
                }
            }
        }
    }
    tracing::info!(%node_id, "channel inbound closed");
}

fn internal<E: std::fmt::Display>(e: E) -> Status {
    Status::internal(e.to_string())
}

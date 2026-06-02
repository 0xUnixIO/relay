//! TCP/UDP port forwarding engine with per-tunnel byte/connection counters.
//!
//! `Engine::apply(ConfigUpdate)` diffs the desired tunnel set against the
//! currently running set, starts/stops tasks accordingly, and returns a
//! ConfigAck. Each running tunnel owns a `CancellationToken` that all of
//! its I/O loops `select!` on so a stop is prompt.
//!
//! `Engine::snapshot()` returns the current counters for every running
//! tunnel; the main loop polls this every few seconds and ships
//! `RuleStats` upstream.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use anyhow::Result;
#[cfg(not(target_os = "linux"))]
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::sync::{Mutex, Semaphore};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

/// 令牌桶限速器。speed_limit_kbps=0 表示不限速。
/// `consume()` 在需要时 sleep，使吞吐量不超过配置速率。
pub struct RateLimiter {
    bps: u64,
    /// (上次填充时刻, 当前可用字节数)
    inner: std::sync::Mutex<(std::time::Instant, f64)>,
}

impl RateLimiter {
    pub fn new(kbps: u64) -> Self {
        let bps = kbps.saturating_mul(1024);
        Self {
            bps,
            inner: std::sync::Mutex::new((std::time::Instant::now(), bps as f64)),
        }
    }

    /// 消耗 `bytes` 个令牌，不足时 sleep 补足时间后返回。
    pub async fn consume(&self, bytes: u64) {
        if self.bps == 0 || bytes == 0 {
            return;
        }
        let sleep_dur = {
            let mut guard = self.inner.lock().unwrap();
            let (epoch, available) = &mut *guard;
            let now = std::time::Instant::now();
            let refill = now.duration_since(*epoch).as_secs_f64() * self.bps as f64;
            // 桶容量上限 = 1 秒突发
            *available = (*available + refill).min(self.bps as f64);
            *epoch = now;

            if *available >= bytes as f64 {
                *available -= bytes as f64;
                return;
            }
            // 令牌不足：算出欠缺多少时间
            let deficit = bytes as f64 - *available;
            *available = 0.0;
            Duration::from_secs_f64(deficit / self.bps as f64)
        };
        tokio::time::sleep(sleep_dur).await;
    }
}

/// Bind a TCP listener.
/// - IPv4 addr → IPv4-only socket.
/// - IPv6 addr + `v6_only=false` → dual-stack (clears IPV6_V6ONLY).
/// - IPv6 addr + `v6_only=true`  → IPv6-only (keeps IPV6_V6ONLY set).
pub fn bind_tcp_listener(addr: std::net::SocketAddr, v6_only: bool) -> Result<TcpListener> {
    use socket2::{Domain, Protocol, Socket, Type};
    let domain = if addr.is_ipv6() {
        Domain::IPV6
    } else {
        Domain::IPV4
    };
    let sock = Socket::new(domain, Type::STREAM, Some(Protocol::TCP))?;
    if addr.is_ipv6() {
        sock.set_only_v6(v6_only)?;
    }
    sock.set_reuse_address(true)?;
    sock.set_nonblocking(true)?;
    sock.bind(&addr.into())?;
    sock.listen(4096)?;
    Ok(TcpListener::from_std(std::net::TcpListener::from(sock))?)
}

/// Same idea for UDP.
pub fn bind_udp_socket(addr: std::net::SocketAddr, v6_only: bool) -> Result<UdpSocket> {
    use socket2::{Domain, Protocol, Socket, Type};
    let domain = if addr.is_ipv6() {
        Domain::IPV6
    } else {
        Domain::IPV4
    };
    let sock = Socket::new(domain, Type::DGRAM, Some(Protocol::UDP))?;
    if addr.is_ipv6() {
        sock.set_only_v6(v6_only)?;
    }
    sock.set_nonblocking(true)?;
    sock.bind(&addr.into())?;
    Ok(UdpSocket::from_std(std::net::UdpSocket::from(sock))?)
}

use relay_proto::v1::{
    node_message::Payload as NodePayload, ClientConnectEvent, ClientDisconnectEvent, ConfigAck,
    ConfigUpdate, ForwardConfig, ForwardStats, NodeMessage, Protocol, UpstreamProbeSample,
};

use crate::acl::Acl;

type LimiterKey = (String, u32);
type SharedLimiter = Arc<Option<RateLimiter>>;

#[derive(Default)]
pub struct Engine {
    running: Mutex<HashMap<String, Running>>,
    /// 每条 (forward_id, hop_index) 共用一个 RateLimiter Arc。
    /// 双协议（TCP+UDP）下两条 listener 拿到同一个桶，合并计费。
    /// 值里附带 bps，spec 改速时可比较是否需要重建。
    rate_limiters: Mutex<HashMap<LimiterKey, (u64, SharedLimiter)>>,
    /// probe 样本缓冲，由 main.rs 定期排空并通过 gRPC 上报。
    pub probe_buf: Arc<Mutex<Vec<UpstreamProbeSample>>>,
    /// 连接建立/断开事件缓冲，由 main.rs 定期排空并通过 gRPC 上报。
    pub conn_event_buf: Arc<Mutex<Vec<NodeMessage>>>,
}

struct Running {
    spec: TunnelSpec,
    /// 上次成功解析的 upstream 地址，供 DDNS 轮询对比。
    resolved_upstreams: Vec<SocketAddr>,
    cancel: CancellationToken,
    handle: tokio::task::JoinHandle<()>,
    counters: Arc<Counters>,
}

#[derive(Default)]
struct Counters {
    bytes_in: AtomicU64,
    bytes_out: AtomicU64,
    active: AtomicU64,
    total: AtomicU64,
    // conn_id -> client_ip（入口跳维护，用于活跃 IP 快照和断开事件）
    active_peers: Mutex<std::collections::HashMap<String, std::net::IpAddr>>,
}

/// 上游连接超时：超过此时长判定节点不可达，立即熔断并故障转移到下一个。
/// 避免坏节点（SYN 无响应）把连接拖到操作系统默认超时（数十秒）。
const UPSTREAM_CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
/// 被动熔断窗口：节点连接失败后，此时间内从轮询中跳过（cooldown 到期自动半开）。
const UPSTREAM_EJECT_COOLDOWN: Duration = Duration::from_secs(15);

/// 上游本地健康状态（被动熔断 / passive outlier detection）。
/// 索引与 routing_upstreams 对齐：数据面连接失败时立即熔断对应节点，
/// 轮询时跳过；cooldown 到期后自动半开，下次成功连接即恢复。
/// 纯被动设计——不依赖 master 下发、也不引入主动探测，TCP/UDP 通用，
/// 因此不会用 TCP 探测误杀只监听 UDP 的上游。
struct Health {
    epoch: std::time::Instant,
    /// 每个上游的熔断到期时间（相对 epoch 的毫秒数）；0 表示健康。
    until_ms: Vec<AtomicU64>,
}

impl Health {
    fn new(n: usize) -> Self {
        Self {
            epoch: std::time::Instant::now(),
            until_ms: (0..n).map(|_| AtomicU64::new(0)).collect(),
        }
    }

    fn now_ms(&self) -> u64 {
        self.epoch.elapsed().as_millis() as u64
    }

    /// 节点当前是否可用于路由（健康，或熔断已到期进入半开）。
    fn is_healthy(&self, idx: usize) -> bool {
        self.until_ms
            .get(idx)
            .map(|u| u.load(Ordering::Relaxed) <= self.now_ms())
            .unwrap_or(true)
    }

    /// 标记节点熔断，cooldown 内从轮询跳过。
    fn mark_down(&self, idx: usize, cooldown: Duration) {
        if let Some(u) = self.until_ms.get(idx) {
            u.store(
                self.now_ms() + cooldown.as_millis() as u64,
                Ordering::Relaxed,
            );
        }
    }

    /// 标记节点恢复健康。
    fn mark_up(&self, idx: usize) {
        if let Some(u) = self.until_ms.get(idx) {
            u.store(0, Ordering::Relaxed);
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct TunnelSpec {
    /// Composite "<forward_id>:<hop_index>" key — uniquely identifies a
    /// listener on this node.
    id: String,
    forward_id: String,
    hop_index: u32,
    protocol: i32,
    listen_addr: String,
    upstream_addrs: Vec<String>,
    lb_strategy: String,
    max_connections: u32,
    enabled: bool,
    /// Bumped on master-driven redeploy → forces spec inequality so we
    /// stop+start the listener even if nothing else changed.
    deploy_generation: u64,
    acl: Acl,
    speed_limit_kbps: u64,
    v6_only: bool,
    /// 被 master 驱逐的 upstream 地址集合：路由时跳过，但仍正常探测以便感知恢复。
    ejected_upstream_addrs: Vec<String>,
}

impl From<&ForwardConfig> for TunnelSpec {
    fn from(t: &ForwardConfig) -> Self {
        let proto_label = match t.protocol {
            x if x == relay_proto::v1::Protocol::Udp as i32 => "udp",
            _ => "tcp",
        };
        Self {
            id: format!("{}:{}:{}", t.forward_id, t.hop_index, proto_label),
            forward_id: t.forward_id.clone(),
            hop_index: t.hop_index,
            protocol: t.protocol,
            listen_addr: t.listen_addr.clone(),
            upstream_addrs: t.upstream_addrs.clone(),
            lb_strategy: t.lb_strategy.clone(),
            max_connections: t.max_connections,
            enabled: t.enabled,
            deploy_generation: t.deploy_generation,
            acl: Acl::new(&t.allow_cidrs, &t.deny_cidrs),
            speed_limit_kbps: t.speed_limit_kbps,
            v6_only: t.v6_only,
            ejected_upstream_addrs: t.ejected_upstream_addrs.clone(),
        }
    }
}

impl Engine {
    pub fn new() -> Self {
        Self::default()
    }

    /// Snapshot per-listener counters as ForwardStats messages.
    /// 双协议同 (forward_id, hop_index) 下两条 listener 的字节数会被合并，
    /// 避免上报到 master 时同 key 互相覆盖造成 delta 错乱。
    pub async fn snapshot(&self) -> Vec<ForwardStats> {
        let g = self.running.lock().await;
        let mut by_key: HashMap<(String, u32), ForwardStats> = HashMap::new();
        for r in g.values() {
            let key = (r.spec.forward_id.clone(), r.spec.hop_index);
            let bytes_in = r.counters.bytes_in.load(Ordering::Relaxed);
            let bytes_out = r.counters.bytes_out.load(Ordering::Relaxed);
            let active = r.counters.active.load(Ordering::Relaxed) as u32;
            let total = r.counters.total.load(Ordering::Relaxed);
            // 只有入口跳（hop_index=0）维护 active_peers
            let active_ips: Vec<String> = if r.spec.hop_index == 0 {
                r.counters
                    .active_peers
                    .lock()
                    .await
                    .values()
                    .map(|ip| ip.to_string())
                    .collect()
            } else {
                vec![]
            };
            by_key
                .entry(key)
                .and_modify(|s| {
                    s.bytes_in = s.bytes_in.saturating_add(bytes_in);
                    s.bytes_out = s.bytes_out.saturating_add(bytes_out);
                    s.active_connections = s.active_connections.saturating_add(active);
                    s.total_connections = s.total_connections.saturating_add(total);
                    // active_ips 直接覆盖（同 forward 同 hop 不会有多条 listener）
                    if !active_ips.is_empty() {
                        s.active_client_ips = active_ips.clone();
                    }
                })
                .or_insert(ForwardStats {
                    forward_id: r.spec.forward_id.clone(),
                    hop_index: r.spec.hop_index,
                    bytes_in,
                    bytes_out,
                    active_connections: active,
                    total_connections: total,
                    active_client_ips: active_ips,
                });
        }
        by_key.into_values().collect()
    }

    pub async fn apply(&self, cfg: ConfigUpdate) -> ConfigAck {
        let version = cfg.version;
        let desired: HashMap<String, TunnelSpec> = cfg
            .forwards
            .iter()
            .filter(|t| t.enabled)
            .map(|t| {
                let s = TunnelSpec::from(t);
                (s.id.clone(), s)
            })
            .collect();

        let mut errors: Vec<String> = Vec::new();
        let mut running = self.running.lock().await;

        let to_stop: Vec<String> = running
            .iter()
            .filter(|(id, r)| match desired.get(*id) {
                None => true,
                Some(d) => *d != r.spec,
            })
            .map(|(id, _)| id.clone())
            .collect();
        // cancel 并等待旧 task 真正退出，确保端口释放后再重新绑定
        let mut stop_handles = Vec::new();
        for id in &to_stop {
            if let Some(r) = running.remove(id) {
                tracing::info!(tunnel = %id, "stopping tunnel");
                r.cancel.cancel();
                stop_handles.push(r.handle);
            }
        }
        for h in stop_handles {
            let _ = h.await;
        }

        for (id, spec) in &desired {
            if running.contains_key(id) {
                continue;
            }
            let cancel = CancellationToken::new();
            let counters = Arc::new(Counters::default());
            let limiter = self
                .get_or_create_limiter(&spec.forward_id, spec.hop_index, spec.speed_limit_kbps)
                .await;
            let resolved = match resolve_upstreams(&spec.upstream_addrs).await {
                Ok(r) => r,
                Err(e) => {
                    let msg = format!("{}: {}", id, e);
                    tracing::error!(tunnel = %id, error = %e, "upstream resolve failed");
                    errors.push(msg);
                    continue;
                }
            };
            match start_tunnel(
                spec.clone(),
                resolved.clone(),
                cancel.clone(),
                counters.clone(),
                limiter,
                self.probe_buf.clone(),
                self.conn_event_buf.clone(),
            )
            .await
            {
                Ok(handle) => {
                    running.insert(
                        id.clone(),
                        Running {
                            spec: spec.clone(),
                            resolved_upstreams: resolved,
                            cancel,
                            handle,
                            counters,
                        },
                    );
                    tracing::info!(tunnel = %id, listen = %spec.listen_addr,
                        upstreams = ?spec.upstream_addrs, strategy = %spec.lb_strategy,
                        proto = spec.protocol, "tunnel started");
                }
                Err(e) => {
                    let msg = format!("{}: {}", id, e);
                    tracing::error!(tunnel = %id, error = %e, "tunnel start failed");
                    errors.push(msg);
                }
            }
        }

        // GC：丢弃没有任何 listener 引用的 rate limiter 条目，避免泄漏。
        // 这里只 drop map 里的 Arc，listener 仍持有的 Arc 会随其结束自然回收。
        {
            let mut limiters = self.rate_limiters.lock().await;
            let alive: std::collections::HashSet<LimiterKey> = running
                .values()
                .map(|r| (r.spec.forward_id.clone(), r.spec.hop_index))
                .collect();
            limiters.retain(|k, _| alive.contains(k));
        }

        ConfigAck {
            config_version: version,
            success: errors.is_empty(),
            error: errors.join("; "),
        }
    }

    /// 取或新建 (forward_id, hop_index) 共享的 RateLimiter。
    /// 同 forward 同 hop 的所有协议 listener 共用一个桶 → 合并限速；
    /// kbps 变化时（spec 不等会触发重建）替换为新桶。
    async fn get_or_create_limiter(
        &self,
        forward_id: &str,
        hop_index: u32,
        kbps: u64,
    ) -> SharedLimiter {
        let key = (forward_id.to_string(), hop_index);
        let mut g = self.rate_limiters.lock().await;
        if let Some((existing_kbps, arc)) = g.get(&key) {
            if *existing_kbps == kbps {
                return arc.clone();
            }
        }
        let arc: SharedLimiter = Arc::new(if kbps > 0 {
            Some(RateLimiter::new(kbps))
        } else {
            None
        });
        g.insert(key, (kbps, arc.clone()));
        arc
    }

    /// 周期性重新解析 hostname 类 upstream 地址。若 IP 变动则重启对应 tunnel。
    /// 纯 IP upstream 不触发额外 DNS 查询。
    pub async fn refresh_dns(&self) {
        let candidates: Vec<(String, TunnelSpec, Vec<SocketAddr>)> = {
            let g = self.running.lock().await;
            g.iter()
                .filter(|(_, r)| {
                    r.spec
                        .upstream_addrs
                        .iter()
                        .any(|a| a.parse::<SocketAddr>().is_err())
                })
                .map(|(id, r)| (id.clone(), r.spec.clone(), r.resolved_upstreams.clone()))
                .collect()
        };

        if candidates.is_empty() {
            return;
        }

        let mut changed: Vec<(String, TunnelSpec, Vec<SocketAddr>)> = Vec::new();
        for (id, spec, old) in candidates {
            match resolve_upstreams(&spec.upstream_addrs).await {
                Ok(new) if new != old => {
                    tracing::info!(tunnel = %id, ?old, ?new, "DNS changed, restarting tunnel");
                    changed.push((id, spec, new));
                }
                Ok(_) => {}
                Err(e) => {
                    tracing::warn!(tunnel = %id, error = %e, "DNS re-resolve failed, keeping current");
                }
            }
        }

        if changed.is_empty() {
            return;
        }

        let mut stop_handles = Vec::new();
        {
            let mut g = self.running.lock().await;
            for (id, _, _) in &changed {
                if let Some(r) = g.remove(id) {
                    r.cancel.cancel();
                    stop_handles.push(r.handle);
                }
            }
        }
        for h in stop_handles {
            let _ = h.await;
        }

        for (id, spec, resolved) in changed {
            let cancel = CancellationToken::new();
            let counters = Arc::new(Counters::default());
            let limiter = self
                .get_or_create_limiter(&spec.forward_id, spec.hop_index, spec.speed_limit_kbps)
                .await;
            match start_tunnel(
                spec.clone(),
                resolved.clone(),
                cancel.clone(),
                counters.clone(),
                limiter,
                self.probe_buf.clone(),
                self.conn_event_buf.clone(),
            )
            .await
            {
                Ok(handle) => {
                    let mut g = self.running.lock().await;
                    g.insert(
                        id.clone(),
                        Running {
                            spec,
                            resolved_upstreams: resolved,
                            cancel,
                            handle,
                            counters,
                        },
                    );
                    tracing::info!(tunnel = %id, "tunnel restarted after DNS change");
                }
                Err(e) => {
                    tracing::error!(tunnel = %id, error = %e, "tunnel restart after DNS change failed");
                }
            }
        }
    }
}

async fn resolve_upstreams(addrs: &[String]) -> Result<Vec<SocketAddr>> {
    if addrs.is_empty() {
        anyhow::bail!("upstream_addrs is empty");
    }
    let mut out = Vec::with_capacity(addrs.len());
    for a in addrs {
        let resolved = tokio::net::lookup_host(a.as_str())
            .await
            .map_err(|e| anyhow::anyhow!("failed to resolve upstream address {a}: {e}"))?
            .next()
            .ok_or_else(|| anyhow::anyhow!("upstream address resolved to nothing: {a}"))?;
        out.push(resolved);
    }
    Ok(out)
}

fn gen_conn_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

async fn start_tunnel(
    spec: TunnelSpec,
    resolved: Vec<SocketAddr>,
    cancel: CancellationToken,
    counters: Arc<Counters>,
    rate_limiter: Arc<Option<RateLimiter>>,
    probe_buf: Arc<Mutex<Vec<UpstreamProbeSample>>>,
    conn_event_buf: Arc<Mutex<Vec<NodeMessage>>>,
) -> Result<tokio::task::JoinHandle<()>> {
    let listen: SocketAddr = spec.listen_addr.parse()?;

    const KNOWN_LB: &[&str] = &[
        "round_robin",
        "random",
        "primary_backup",
        "least_latency",
        "best",
    ];
    if !KNOWN_LB.contains(&spec.lb_strategy.as_str()) && !spec.lb_strategy.is_empty() {
        tracing::warn!(
            tunnel = %spec.id,
            strategy = %spec.lb_strategy,
            "unknown lb_strategy; falling back to round_robin"
        );
    }

    // probe_upstreams = 全量（含被驱逐），用于探测和恢复感知
    // routing_upstreams = 健康子集，用于实际路由；若全部被驱逐则降级为全量
    // routing_to_probe = routing_upstreams 中每个元素在 probe_upstreams 中的索引
    let probe_upstreams = Arc::new(resolved);
    let ejected_set: std::collections::HashSet<&str> = spec
        .ejected_upstream_addrs
        .iter()
        .map(|s| s.as_str())
        .collect();
    let (routing_resolved, routing_to_probe_raw): (Vec<SocketAddr>, Vec<usize>) = spec
        .upstream_addrs
        .iter()
        .enumerate()
        .zip(probe_upstreams.iter())
        .filter(|((_, addr), _)| !ejected_set.contains(addr.as_str()))
        .map(|((i, _), &sa)| (sa, i))
        .unzip();
    let (routing_upstreams, routing_to_probe) = if routing_resolved.is_empty() {
        // 全部被驱逐：降级使用全量，probe index = identity
        let n = probe_upstreams.len();
        (
            probe_upstreams.clone(),
            Arc::new((0..n).collect::<Vec<_>>()),
        )
    } else {
        (Arc::new(routing_resolved), Arc::new(routing_to_probe_raw))
    };

    let cursor = Arc::new(AtomicUsize::new(0));
    // 上游健康状态，索引与 routing_upstreams 对齐，被动熔断用。
    let health = Arc::new(Health::new(routing_upstreams.len()));

    // EMA 延迟缓存：按 probe_upstreams 索引，探测任务写入，路由通过 routing_to_probe 查询。
    // u64::MAX 表示暂无数据；alpha = 1/4：new_ema = (sample + old * 3) / 4
    const PROBE_FAILURE_US: u64 = 5_000_000; // 失败探测惩罚 5s
    let latency_map: Arc<RwLock<Vec<u64>>> =
        Arc::new(RwLock::new(vec![u64::MAX; probe_upstreams.len()]));

    // 后台 probe 任务：探测全量 upstream（含被驱逐的），上报给 master 以感知恢复。
    {
        let upstreams = probe_upstreams.clone();
        let upstream_addrs = spec.upstream_addrs.clone();
        let forward_id = spec.forward_id.clone();
        let probe_buf = probe_buf.clone();
        let cancel = cancel.clone();
        let latency_map = latency_map.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(300));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                // 首次 tick 立即触发（interval 第一个 tick 无延迟），后续每 300s 一次
                tokio::select! {
                    _ = cancel.cancelled() => break,
                    _ = interval.tick() => {
                        for (idx, &upstream) in upstreams.iter().enumerate() {
                            let t0 = std::time::Instant::now();
                            let result = tokio::time::timeout(
                                Duration::from_secs(5),
                                TcpStream::connect(upstream),
                            ).await;
                            // latency_us=0 上报给 master 表示失败（master 存 NULL）
                            let latency_us = match &result {
                                Ok(Ok(_)) => t0.elapsed().as_micros() as u64,
                                _ => 0,
                            };
                            // EMA 用 result 判断成功/失败，避免把极低延迟（<1μs）误判为失败
                            let sample_us = match &result {
                                Ok(Ok(_)) => latency_us.max(1),
                                _ => PROBE_FAILURE_US,
                            };
                            if let Ok(mut map) = latency_map.write() {
                                let old = map[idx];
                                map[idx] = if old == u64::MAX {
                                    sample_us
                                } else {
                                    (sample_us + old.saturating_mul(3)) / 4
                                };
                            }
                            let addr = upstream_addrs.get(idx)
                                .cloned()
                                .unwrap_or_else(|| upstream.to_string());
                            let sample = UpstreamProbeSample {
                                forward_id: forward_id.clone(),
                                upstream_addr: addr,
                                latency_us,
                                ts_unix_ms: std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .map(|d| d.as_millis() as i64)
                                    .unwrap_or(0),
                            };
                            probe_buf.lock().await.push(sample);
                        }
                    }
                }
            }
        });
    }

    let handle = match Protocol::try_from(spec.protocol).unwrap_or(Protocol::Tcp) {
        Protocol::Tcp => {
            let listener = bind_tcp_listener(listen, spec.v6_only)?;
            let sem = if spec.max_connections > 0 {
                Some(Arc::new(Semaphore::new(spec.max_connections as usize)))
            } else {
                None
            };
            let is_entry = spec.hop_index == 0;
            tokio::spawn(run_tcp(
                spec.id.clone(),
                spec.forward_id.clone(),
                listener,
                routing_upstreams,
                routing_to_probe,
                cursor,
                sem,
                cancel,
                counters,
                spec.acl.clone(),
                rate_limiter,
                spec.lb_strategy.clone(),
                latency_map,
                is_entry,
                conn_event_buf,
                health,
            ))
        }
        Protocol::Udp => {
            let sock = bind_udp_socket(listen, spec.v6_only)?;
            tokio::spawn(run_udp(
                spec.id.clone(),
                sock,
                routing_upstreams,
                routing_to_probe,
                cursor,
                cancel,
                counters,
                spec.acl.clone(),
                rate_limiter,
                spec.lb_strategy.clone(),
                latency_map,
                health,
            ))
        }
    };
    Ok(handle)
}

#[allow(clippy::too_many_arguments)]
async fn run_tcp(
    id: String,
    forward_id: String,
    listener: TcpListener,
    upstreams: Arc<Vec<SocketAddr>>,
    routing_to_probe: Arc<Vec<usize>>,
    cursor: Arc<AtomicUsize>,
    sem: Option<Arc<Semaphore>>,
    cancel: CancellationToken,
    counters: Arc<Counters>,
    acl: Acl,
    rate_limiter: Arc<Option<RateLimiter>>,
    lb_strategy: String,
    latency_map: Arc<RwLock<Vec<u64>>>,
    is_entry: bool,
    conn_event_buf: Arc<Mutex<Vec<NodeMessage>>>,
    health: Arc<Health>,
) {
    loop {
        let permit = if let Some(ref s) = sem {
            match s.clone().acquire_owned().await {
                Ok(p) => Some(p),
                Err(_) => break,
            }
        } else {
            None
        };
        tokio::select! {
            _ = cancel.cancelled() => break,
            r = listener.accept() => {
                match r {
                    Ok((inbound, peer)) => {
                        if !acl.permits(peer.ip()) {
                            tracing::debug!(tunnel = %id, %peer, "rejecting peer (acl)");
                            drop(permit);
                            drop(inbound);
                            continue;
                        }
                        let conn_id = gen_conn_id();
                        let ts_unix_ms = now_ms();

                        // 入口跳：记录 active_peers + 发 ConnectEvent
                        if is_entry {
                            counters.active_peers.lock().await
                                .insert(conn_id.clone(), peer.ip());
                            conn_event_buf.lock().await.push(NodeMessage {
                                payload: Some(NodePayload::ClientConnect(ClientConnectEvent {
                                    forward_id: forward_id.clone(),
                                    hop_index: 0,
                                    conn_id: conn_id.clone(),
                                    client_ip: peer.ip().to_string(),
                                    ts_unix_ms,
                                })),
                            });
                        }

                        let cancel = cancel.clone();
                        let id = id.clone();
                        let counters = counters.clone();
                        let upstreams = upstreams.clone();
                        let rtp = routing_to_probe.clone();
                        let cursor = cursor.clone();
                        let rate_limiter = rate_limiter.clone();
                        let lb = lb_strategy.clone();
                        let lmap = latency_map.clone();
                        let health = health.clone();
                        let conn_event_buf2 = conn_event_buf.clone();
                        counters.total.fetch_add(1, Ordering::Relaxed);
                        counters.active.fetch_add(1, Ordering::Relaxed);
                        tokio::spawn(async move {
                            let _permit = permit;
                            // 记录连接开始时的字节数，用于计算本次连接的增量
                            let bytes_in_before  = counters.bytes_in.load(Ordering::Relaxed);
                            let bytes_out_before = counters.bytes_out.load(Ordering::Relaxed);
                            let res = pipe_tcp(inbound, upstreams, &rtp, cursor, &counters, &cancel, &rate_limiter, &lb, &lmap, &health).await;
                            counters.active.fetch_sub(1, Ordering::Relaxed);

                            if is_entry {
                                counters.active_peers.lock().await.remove(&conn_id);
                                let d_in  = counters.bytes_in.load(Ordering::Relaxed)
                                    .saturating_sub(bytes_in_before);
                                let d_out = counters.bytes_out.load(Ordering::Relaxed)
                                    .saturating_sub(bytes_out_before);
                                conn_event_buf2.lock().await.push(NodeMessage {
                                    payload: Some(NodePayload::ClientDisconnect(ClientDisconnectEvent {
                                        conn_id,
                                        ts_unix_ms: now_ms(),
                                        bytes_in:  d_in,
                                        bytes_out: d_out,
                                    })),
                                });
                            }

                            if let Err(e) = res {
                                tracing::debug!(tunnel = %id, %peer, error = %e, "tcp session ended");
                            }
                        });
                    }
                    Err(e) => {
                        tracing::warn!(tunnel = %id, error = %e, "accept failed");
                        tokio::time::sleep(Duration::from_millis(100)).await;
                    }
                }
            }
        }
    }
    tracing::info!(tunnel = %id, "tcp listener stopped");
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// 对 TcpStream 启用 keepalive：60s 空闲后开始探测，10s 间隔，Linux 下最多 3 次。
/// 探测失败后内核关闭连接，fd 立即回收，避免客户端断网后死连接长达 2 小时。
fn apply_keepalive(stream: &TcpStream) -> std::io::Result<()> {
    use socket2::{SockRef, TcpKeepalive};
    let ka = TcpKeepalive::new()
        .with_time(Duration::from_secs(60))
        .with_interval(Duration::from_secs(10));
    #[cfg(target_os = "linux")]
    let ka = ka.with_retries(3);
    SockRef::from(stream).set_tcp_keepalive(&ka)
}

#[allow(clippy::too_many_arguments)]
async fn pipe_tcp(
    inbound: TcpStream,
    upstreams: Arc<Vec<SocketAddr>>,
    routing_to_probe: &[usize],
    cursor: Arc<AtomicUsize>,
    counters: &Counters,
    cancel: &CancellationToken,
    rate_limiter: &Arc<Option<RateLimiter>>,
    lb_strategy: &str,
    latency_map: &RwLock<Vec<u64>>,
    health: &Health,
) -> std::io::Result<()> {
    inbound.set_nodelay(true)?;
    apply_keepalive(&inbound)?;
    let n = upstreams.len();
    if n == 0 {
        return Err(std::io::Error::other("no upstreams configured"));
    }
    let start = match lb_strategy {
        "primary_backup" => 0,
        "least_latency" | "best" => {
            // 通过 routing_to_probe 映射查 EMA 延迟，冷启动期降级为 round-robin
            latency_map
                .read()
                .ok()
                .and_then(|m| {
                    if routing_to_probe
                        .iter()
                        .all(|&pi| m.get(pi).copied().unwrap_or(u64::MAX) == u64::MAX)
                    {
                        return None;
                    }
                    routing_to_probe
                        .iter()
                        .enumerate()
                        .min_by_key(|(_, &pi)| m.get(pi).copied().unwrap_or(u64::MAX))
                        .map(|(i, _)| i)
                })
                .unwrap_or_else(|| cursor.fetch_add(1, Ordering::Relaxed) % n)
        }
        _ => cursor.fetch_add(1, Ordering::Relaxed) % n,
    };

    let up = {
        // 从 start 起轮转的完整顺序；健康节点优先尝试，熔断中的排到最后，
        // 仅当所有节点都熔断时才兜底尝试——避免每次都先撞坏节点再超时。
        let order = (0..n).map(|i| (start + i) % n);
        let (mut attempt, mut downed): (Vec<usize>, Vec<usize>) =
            order.partition(|&idx| health.is_healthy(idx));
        attempt.append(&mut downed);

        let mut last_err: Option<std::io::Error> = None;
        let mut connected: Option<TcpStream> = None;
        for idx in attempt {
            let upstream = upstreams[idx];
            match tokio::time::timeout(UPSTREAM_CONNECT_TIMEOUT, TcpStream::connect(upstream)).await
            {
                Ok(Ok(stream)) => {
                    stream.set_nodelay(true)?;
                    apply_keepalive(&stream)?;
                    health.mark_up(idx);
                    connected = Some(stream);
                    break;
                }
                Ok(Err(e)) => {
                    tracing::warn!(%upstream, error = %e, "upstream connect failed, ejecting & trying next");
                    health.mark_down(idx, UPSTREAM_EJECT_COOLDOWN);
                    last_err = Some(e);
                }
                Err(_) => {
                    tracing::warn!(%upstream, timeout = ?UPSTREAM_CONNECT_TIMEOUT, "upstream connect timed out, ejecting & trying next");
                    health.mark_down(idx, UPSTREAM_EJECT_COOLDOWN);
                    last_err = Some(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        "upstream connect timed out",
                    ));
                }
            }
        }
        connected.ok_or_else(|| {
            last_err.unwrap_or_else(|| std::io::Error::other("all upstreams unreachable"))
        })?
    };

    #[cfg(target_os = "linux")]
    return pipe_tcp_splice(inbound, up, counters, cancel, rate_limiter).await;
    #[cfg(not(target_os = "linux"))]
    pipe_tcp_copy(inbound, up, counters, cancel, rate_limiter).await
}

/// 半关闭 grace 窗口：一侧 EOF 后，另一侧最多再传 30s 尾部数据；
/// 超时则放弃，确保 TcpStream 被 drop、socket 不会卡在 CLOSE-WAIT。
const HALF_CLOSE_GRACE: Duration = Duration::from_secs(30);

#[cfg(not(target_os = "linux"))]
async fn pipe_tcp_copy(
    inbound: TcpStream,
    up: TcpStream,
    counters: &Counters,
    cancel: &CancellationToken,
    rate_limiter: &Arc<Option<RateLimiter>>,
) -> std::io::Result<()> {
    let (mut ri, mut wi) = inbound.into_split();
    let (mut ro, mut wo) = up.into_split();

    let rl = rate_limiter.clone();
    let a = async {
        let mut buf = vec![0u8; 16 * 1024];
        loop {
            let n = ri.read(&mut buf).await?;
            if n == 0 {
                break;
            }
            counters.bytes_in.fetch_add(n as u64, Ordering::Relaxed);
            if let Some(rl) = rl.as_ref() {
                rl.consume(n as u64).await;
            }
            wo.write_all(&buf[..n]).await?;
        }
        let _ = wo.shutdown().await;
        Ok::<_, std::io::Error>(())
    };
    let rl = rate_limiter.clone();
    let b = async {
        let mut buf = vec![0u8; 16 * 1024];
        loop {
            let n = ro.read(&mut buf).await?;
            if n == 0 {
                break;
            }
            counters.bytes_out.fetch_add(n as u64, Ordering::Relaxed);
            if let Some(rl) = rl.as_ref() {
                rl.consume(n as u64).await;
            }
            wi.write_all(&buf[..n]).await?;
        }
        let _ = wi.shutdown().await;
        Ok::<_, std::io::Error>(())
    };

    // cancel 分支 + 半关闭 grace：任一方向先结束，另一方向最多再跑
    // HALF_CLOSE_GRACE，防止对端不主动 close 导致 socket 永不释放。
    let a = async {
        tokio::select! {
            _ = cancel.cancelled() => Ok::<_, std::io::Error>(()),
            r = a => r,
        }
    };
    let b = async {
        tokio::select! {
            _ = cancel.cancelled() => Ok::<_, std::io::Error>(()),
            r = b => r,
        }
    };
    tokio::pin!(a, b);
    tokio::select! {
        r = &mut a => {
            let _ = tokio::time::timeout(HALF_CLOSE_GRACE, &mut b).await;
            r
        }
        r = &mut b => {
            let _ = tokio::time::timeout(HALF_CLOSE_GRACE, &mut a).await;
            r
        }
    }
}

/// Linux zero-copy path: splice between socket and pipe (kernel buffer),
/// avoiding user-space data copies. Per-direction byte counters are still
/// accurate because splice returns the bytes moved.
#[cfg(target_os = "linux")]
async fn pipe_tcp_splice(
    inbound: TcpStream,
    up: TcpStream,
    counters: &Counters,
    cancel: &CancellationToken,
    rate_limiter: &Arc<Option<RateLimiter>>,
) -> std::io::Result<()> {
    let a = splice_one_way(&inbound, &up, &counters.bytes_in, cancel, rate_limiter);
    let b = splice_one_way(&up, &inbound, &counters.bytes_out, cancel, rate_limiter);
    tokio::pin!(a, b);

    // 半关闭 grace：任一方向 EOF/Err 后，另一方向最多再跑 HALF_CLOSE_GRACE。
    // 防止 upstream 主动关连接、client 不主动 close 时整条 task 永不退出，
    // 导致 TcpStream 不 drop、socket 卡在 CLOSE-WAIT 泄漏。
    tokio::select! {
        r = &mut a => {
            let _ = tokio::time::timeout(HALF_CLOSE_GRACE, &mut b).await;
            r
        }
        r = &mut b => {
            let _ = tokio::time::timeout(HALF_CLOSE_GRACE, &mut a).await;
            r
        }
    }
}

#[cfg(target_os = "linux")]
async fn splice_one_way(
    src: &TcpStream,
    dst: &TcpStream,
    counter: &AtomicU64,
    cancel: &CancellationToken,
    rate_limiter: &Arc<Option<RateLimiter>>,
) -> std::io::Result<()> {
    use nix::fcntl::{splice, OFlag, SpliceFFlags};
    use nix::unistd::pipe2;
    use std::os::fd::{AsFd, AsRawFd};
    use tokio::io::Interest;

    let (pipe_r, pipe_w) = pipe2(OFlag::O_NONBLOCK | OFlag::O_CLOEXEC)
        .map_err(|e| std::io::Error::from_raw_os_error(e as i32))?;
    let flags = SpliceFFlags::SPLICE_F_NONBLOCK | SpliceFFlags::SPLICE_F_MOVE;
    const CHUNK: usize = 64 * 1024;

    loop {
        // Wait for src readability (cancel-safe) then attempt splice src -> pipe.
        tokio::select! {
            _ = cancel.cancelled() => return Ok(()),
            r = src.readable() => r?,
        }
        let n = match src.try_io(Interest::READABLE, || {
            splice(src.as_fd(), None, pipe_w.as_fd(), None, CHUNK, flags)
                .map_err(std::io::Error::from)
        }) {
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => continue,
            Err(e) => return Err(e),
        };
        if n == 0 {
            // Source EOF: half-close dst's write side so peer sees FIN.
            // SAFETY: dst is a live TcpStream; SHUT_WR on a valid socket fd is sound.
            unsafe { libc::shutdown(dst.as_raw_fd(), libc::SHUT_WR) };
            break;
        }
        counter.fetch_add(n as u64, Ordering::Relaxed);
        if let Some(rl) = rate_limiter.as_ref() {
            rl.consume(n as u64).await;
        }

        // Drain the n bytes we just put into the pipe to dst.
        let mut remaining = n;
        while remaining > 0 {
            tokio::select! {
                _ = cancel.cancelled() => return Ok(()),
                r = dst.writable() => r?,
            }
            match dst.try_io(Interest::WRITABLE, || {
                splice(pipe_r.as_fd(), None, dst.as_fd(), None, remaining, flags)
                    .map_err(std::io::Error::from)
            }) {
                Ok(0) => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::WriteZero,
                        "splice to dst returned 0",
                    ))
                }
                Ok(m) => remaining -= m,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => continue,
                Err(e) => return Err(e),
            }
        }
    }
    Ok(())
}

const UDP_IDLE: Duration = Duration::from_secs(60);
const UDP_MAX_SESSIONS: usize = 1024;

#[allow(clippy::too_many_arguments)]
async fn run_udp(
    id: String,
    sock: UdpSocket,
    upstreams: Arc<Vec<SocketAddr>>,
    routing_to_probe: Arc<Vec<usize>>,
    cursor: Arc<AtomicUsize>,
    cancel: CancellationToken,
    counters: Arc<Counters>,
    acl: Acl,
    rate_limiter: Arc<Option<RateLimiter>>,
    lb_strategy: String,
    latency_map: Arc<RwLock<Vec<u64>>>,
    health: Arc<Health>,
) {
    let sock = Arc::new(sock);
    let sessions: Arc<Mutex<HashMap<SocketAddr, UdpSession>>> =
        Arc::new(Mutex::new(HashMap::new()));

    {
        let sessions = sessions.clone();
        let cancel = cancel.clone();
        let id = id.clone();
        let counters = counters.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(10));
            loop {
                tokio::select! {
                    _ = cancel.cancelled() => break,
                    _ = tick.tick() => {
                        let mut g = sessions.lock().await;
                        let now = Instant::now();
                        let mut to_remove = Vec::new();
                        for (addr, sess) in g.iter() {
                            if now.duration_since(sess.last_seen) > UDP_IDLE {
                                to_remove.push(*addr);
                            }
                        }
                        for addr in to_remove {
                            if let Some(s) = g.remove(&addr) {
                                s.cancel.cancel();
                                counters.active.fetch_sub(1, Ordering::Relaxed);
                                tracing::debug!(tunnel = %id, %addr, "udp session evicted");
                            }
                        }
                    }
                }
            }
        });
    }

    let mut buf = vec![0u8; 64 * 1024];
    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            r = sock.recv_from(&mut buf) => {
                let (n, peer) = match r {
                    Ok(v) => v,
                    Err(e) => {
                        tracing::warn!(tunnel = %id, error = %e, "udp recv failed");
                        continue;
                    }
                };
                if !acl.permits(peer.ip()) {
                    tracing::debug!(tunnel = %id, %peer, "rejecting udp peer (acl)");
                    continue;
                }
                counters.bytes_in.fetch_add(n as u64, Ordering::Relaxed);
                if let Some(rl) = rate_limiter.as_ref() {
                    rl.consume(n as u64).await;
                }

                let mut g = sessions.lock().await;
                let entry = if let Some(s) = g.get_mut(&peer) {
                    s.last_seen = Instant::now();
                    Some(s.up.clone())
                } else if g.len() >= UDP_MAX_SESSIONS {
                    tracing::warn!(tunnel = %id, "udp session cap reached, dropping packet");
                    None
                } else if upstreams.is_empty() {
                    tracing::warn!(tunnel = %id, "no upstreams, dropping udp packet");
                    None
                } else {
                    let n = upstreams.len();
                    let mut up_idx = match lb_strategy.as_str() {
                        "primary_backup" => 0,
                        "least_latency" | "best" => {
                            latency_map.read().ok().and_then(|m| {
                                if routing_to_probe.iter().all(|&pi| m.get(pi).copied().unwrap_or(u64::MAX) == u64::MAX) { return None; }
                                routing_to_probe.iter().enumerate()
                                    .min_by_key(|(_, &pi)| m.get(pi).copied().unwrap_or(u64::MAX))
                                    .map(|(i, _)| i)
                            }).unwrap_or_else(|| cursor.fetch_add(1, Ordering::Relaxed) % n)
                        }
                        _ => cursor.fetch_add(1, Ordering::Relaxed) % n,
                    };
                    // 被动熔断：选中节点若在熔断窗口内，顺延到下一个健康节点；
                    // 全部熔断时保持原选择兜底。
                    if !health.is_healthy(up_idx) {
                        if let Some(alt) = (0..n).map(|k| (up_idx + k) % n).find(|&i| health.is_healthy(i)) {
                            up_idx = alt;
                        }
                    }
                    let upstream = upstreams[up_idx];
                    let bind: SocketAddr = if upstream.is_ipv4() {
                        "0.0.0.0:0".parse().unwrap()
                    } else {
                        "[::]:0".parse().unwrap()
                    };
                    match UdpSocket::bind(bind).await {
                        Ok(up) => match up.connect(upstream).await {
                            Ok(()) => {
                                health.mark_up(up_idx);
                                let up = Arc::new(up);
                                let sess_cancel = CancellationToken::new();
                                let sock2 = sock.clone();
                                let up2 = up.clone();
                                let cancel2 = sess_cancel.clone();
                                let id2 = id.clone();
                                let counters2 = counters.clone();
                                counters.total.fetch_add(1, Ordering::Relaxed);
                                counters.active.fetch_add(1, Ordering::Relaxed);
                                let rate_limiter2 = rate_limiter.clone();
                                let health2 = health.clone();
                                tokio::spawn(async move {
                                    let mut rb = vec![0u8; 64 * 1024];
                                    loop {
                                        tokio::select! {
                                            _ = cancel2.cancelled() => break,
                                            r = up2.recv(&mut rb) => match r {
                                                Ok(n) => {
                                                    counters2.bytes_out.fetch_add(n as u64, Ordering::Relaxed);
                                                    if let Some(rl) = rate_limiter2.as_ref() {
                                                        rl.consume(n as u64).await;
                                                    }
                                                    if let Err(e) = sock2.send_to(&rb[..n], peer).await {
                                                        tracing::debug!(tunnel = %id2,
                                                            error = %e, "udp reply send failed");
                                                        break;
                                                    }
                                                }
                                                Err(e) => {
                                                    // connected UDP 上的 recv 错误通常是 ICMP
                                                    // 端口不可达（ECONNREFUSED），被动熔断该节点。
                                                    health2.mark_down(up_idx, UPSTREAM_EJECT_COOLDOWN);
                                                    tracing::debug!(tunnel = %id2,
                                                        error = %e, "udp upstream recv ended, ejecting");
                                                    break;
                                                }
                                            }
                                        }
                                    }
                                });
                                g.insert(peer, UdpSession {
                                    up: up.clone(),
                                    last_seen: Instant::now(),
                                    cancel: sess_cancel,
                                });
                                Some(up)
                            }
                            Err(e) => {
                                health.mark_down(up_idx, UPSTREAM_EJECT_COOLDOWN);
                                tracing::warn!(tunnel = %id, error = %e, "udp upstream connect failed, ejecting");
                                None
                            }
                        },
                        Err(e) => {
                            tracing::warn!(tunnel = %id, error = %e, "udp bind failed");
                            None
                        }
                    }
                };
                drop(g);

                if let Some(up) = entry {
                    if let Err(e) = up.send(&buf[..n]).await {
                        tracing::debug!(tunnel = %id, %peer, error = %e, "udp upstream send failed");
                    }
                }
            }
        }
    }
    let mut g = sessions.lock().await;
    for (_, s) in g.drain() {
        s.cancel.cancel();
    }
    tracing::info!(tunnel = %id, "udp listener stopped");
}

struct UdpSession {
    up: Arc<UdpSocket>,
    last_seen: Instant,
    cancel: CancellationToken,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 复现"不通的上游仍被轮询过去"：节点被熔断后必须从可用集合中跳过。
    #[test]
    fn ejected_upstream_is_skipped() {
        let h = Health::new(3);
        // 初始全部健康
        assert!((0..3).all(|i| h.is_healthy(i)));
        // 熔断节点 1
        h.mark_down(1, Duration::from_secs(15));
        assert!(h.is_healthy(0));
        assert!(!h.is_healthy(1), "熔断的节点不应再被判为健康");
        assert!(h.is_healthy(2));
    }

    /// 健康优先排序：熔断节点排到最后，仅作兜底。这正是 pipe_tcp 的选路顺序。
    #[test]
    fn unhealthy_nodes_ordered_last() {
        let h = Health::new(4);
        h.mark_down(0, Duration::from_secs(15));
        h.mark_down(2, Duration::from_secs(15));
        let n = 4usize;
        let start = 0usize;
        let order = (0..n).map(|i| (start + i) % n);
        let (mut attempt, mut downed): (Vec<usize>, Vec<usize>) =
            order.partition(|&idx| h.is_healthy(idx));
        attempt.append(&mut downed);
        // 健康的 1、3 先于熔断的 0、2
        assert_eq!(attempt, vec![1, 3, 0, 2]);
    }

    /// cooldown 到期后自动半开恢复（无需后台任务）。
    #[test]
    fn cooldown_expires_to_half_open() {
        let h = Health::new(1);
        h.mark_down(0, Duration::from_millis(50));
        assert!(!h.is_healthy(0));
        std::thread::sleep(Duration::from_millis(80));
        assert!(h.is_healthy(0), "cooldown 到期后应自动半开");
    }

    /// mark_up 立即恢复（成功连接路径）。
    #[test]
    fn mark_up_restores_immediately() {
        let h = Health::new(2);
        h.mark_down(0, Duration::from_secs(60));
        assert!(!h.is_healthy(0));
        h.mark_up(0);
        assert!(h.is_healthy(0));
    }
}

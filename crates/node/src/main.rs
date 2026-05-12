use anyhow::{anyhow, Context, Result};
use clap::Parser;
use relay_proto::v1::{
    master_message::Payload as MasterPayload, node_service_client::NodeServiceClient, Heartbeat,
    NodeMessage, ProbeRequest, ProbeResult, RenewCertRequest, RenewCertResponse,
};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use time::OffsetDateTime;
use tokio::sync::{mpsc, oneshot, Mutex, Notify};
use tokio_stream::wrappers::ReceiverStream;
use tonic::transport::{Certificate, ClientTlsConfig, Endpoint, Identity};

mod acl;
mod enroll;
mod forward;
mod upgrade;

#[derive(Debug, Parser)]
#[command(name = "relay-node", version, about = "Relay forwarding agent")]
struct Cli {
    /// Node id (must match the cert SAN issued by the master).
    #[arg(long, env = "NODE_ID")]
    node_id: String,

    /// Master gRPC endpoint (e.g. https://master.example.com:7443).
    #[arg(long, env = "NODE_MASTER_ENDPOINT")]
    master: String,

    /// Directory holding ca.crt + node.crt + node.key.
    #[arg(long, env = "NODE_PKI_DIR", default_value = "/var/lib/relay-node/pki")]
    pki_dir: PathBuf,

    /// Override the TLS server name used to validate the master cert.
    /// Defaults to the host part of `--master`.
    #[arg(long, env = "NODE_MASTER_SERVER_NAME")]
    master_server_name: Option<String>,

    // --- cold-start enrollment (only consumed when pki_dir is empty) ---
    /// Master HTTP enroll endpoint (defaults to `--master` host on port 7080).
    #[arg(long, env = "NODE_MASTER_ENROLL_ENDPOINT")]
    master_enroll_endpoint: Option<String>,

    /// One-time enrollment token issued by the master.
    #[arg(long, env = "NODE_TOKEN")]
    token: Option<String>,

    /// Base64-encoded master CA cert, baked into the install command so we
    /// never have to TOFU-trust the enrollment endpoint.
    #[arg(long, env = "NODE_CA_CERT_B64")]
    ca_cert_b64: Option<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    // 将进程软 fd 限制拉满到系统硬限制，避免高并发时因默认 1024 耗尽 fd
    #[cfg(unix)]
    unsafe {
        let mut rl = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        if libc::getrlimit(libc::RLIMIT_NOFILE, &mut rl) == 0 {
            rl.rlim_cur = rl.rlim_max;
            libc::setrlimit(libc::RLIMIT_NOFILE, &rl);
        }
    }

    let _ = dotenvy::dotenv();
    relay_common::logging::init("relay-node");

    let cli = Cli::parse();
    tracing::info!(
        node_id = %cli.node_id,
        master = %cli.master,
        pki_dir = %cli.pki_dir.display(),
        "starting relay-node"
    );

    if !enroll::pki_complete(&cli.pki_dir) {
        cold_start_enroll(&cli).await?;
    }

    // Cross-task plumbing for in-channel cert renewal. One instance lives for
    // the whole process; each session swaps its outbound sender in/out.
    let cert_session = Arc::new(CertSession::default());

    {
        let cert_session = cert_session.clone();
        let pki_dir = cli.pki_dir.clone();
        let node_id = cli.node_id.clone();
        tokio::spawn(async move {
            cert_renewer(node_id, pki_dir, cert_session).await;
        });
    }

    // Reconnect loop: any error or stream EOF (e.g. master upgrade) should
    // bring us back, with capped exponential backoff. Only enrollment
    // failures fall outside this loop. A successful cert rotation also
    // triggers a graceful reconnect via `cert_session.reconnect`.
    let engine = Arc::new(forward::Engine::new());
    let mut backoff = Duration::from_secs(1);
    let max_backoff = Duration::from_secs(30);
    loop {
        match run_session(&cli, engine.clone(), cert_session.clone()).await {
            Ok(reason) => {
                tracing::warn!(
                    ?reason,
                    "master stream closed, reconnecting in {:?}",
                    backoff
                );
                // 正常关闭（EOF 或证书轮转）说明连接曾成功建立，快速重连
                backoff = Duration::from_secs(1);
            }
            Err(e) => {
                tracing::warn!(error = ?e, "master session error, reconnecting in {:?}", backoff);
            }
        }
        tokio::time::sleep(backoff).await;
        backoff = std::cmp::min(backoff * 2, max_backoff);
    }
}

#[derive(Debug)]
enum SessionExit {
    Eof,
    CertRotated,
}

#[derive(Default)]
struct CertSession {
    /// Current session's outbound sender, populated for the lifetime of one
    /// mTLS Channel. The renewer reads this to dispatch a RenewCertRequest.
    outbound: Mutex<Option<mpsc::Sender<NodeMessage>>>,
    /// Set by the renewer just before sending a request; the inbound loop
    /// completes it when the matching RenewCertResponse arrives.
    pending: Mutex<Option<oneshot::Sender<RenewCertResponse>>>,
    /// Pulsed after the renewer has installed a new cert+key on disk so the
    /// session loop drops its current Channel and reconnects with the new
    /// identity.
    reconnect: Notify,
}

async fn run_session(
    cli: &Cli,
    engine: Arc<forward::Engine>,
    cert_session: Arc<CertSession>,
) -> Result<SessionExit> {
    let mut client = connect_mtls(cli).await?;

    let (tx, rx) = tokio::sync::mpsc::channel::<NodeMessage>(64);
    let channel_req = tonic::Request::new(ReceiverStream::new(rx));
    let mut inbound = client.channel(channel_req).await?.into_inner();

    *cert_session.outbound.lock().await = Some(tx.clone());

    let hb_tx = tx.clone();
    let hb_engine = engine.clone();
    let interval = Duration::from_millis(5_000);
    let hb_handle = tokio::spawn(async move {
        // sysinfo needs to be refreshed twice ~250ms apart for the first
        // CPU sample to be meaningful; do it inside the task so we don't
        // block startup.
        let mut sys = sysinfo::System::new();
        sys.refresh_memory();
        sys.refresh_cpu_usage();
        tokio::time::sleep(Duration::from_millis(300)).await;

        let mut tick = tokio::time::interval(interval);
        loop {
            tick.tick().await;
            sys.refresh_cpu_usage();
            sys.refresh_memory();
            let cpu_pct = sys.cpus().iter().map(|c| c.cpu_usage()).sum::<f32>()
                / sys.cpus().len().max(1) as f32;
            let mem_total = sys.total_memory();
            let mem_used = sys.used_memory();

            let active = hb_engine
                .snapshot()
                .await
                .iter()
                .map(|s| s.active_connections as u64)
                .sum::<u64>() as u32;

            let hb = NodeMessage {
                payload: Some(relay_proto::v1::node_message::Payload::Heartbeat(
                    Heartbeat {
                        ts_unix_ms: now_ms(),
                        cpu_pct: cpu_pct as f64,
                        mem_used_bytes: mem_used,
                        mem_total_bytes: mem_total,
                        active_connections: active,
                        agent_version: env!("CARGO_PKG_VERSION").to_string(),
                        protocol_version: relay_common::PROTOCOL_VERSION,
                        capabilities: vec!["upgrade_v1".to_string()],
                    },
                )),
            };
            if hb_tx.send(hb).await.is_err() {
                break;
            }
        }
    });

    // Periodic RuleStats reporter.
    let stats_tx = tx.clone();
    let stats_engine = engine.clone();
    let stats_handle = tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(30));
        tick.tick().await;
        loop {
            tick.tick().await;
            for s in stats_engine.snapshot().await {
                let msg = NodeMessage {
                    payload: Some(relay_proto::v1::node_message::Payload::Stats(s)),
                };
                if stats_tx.send(msg).await.is_err() {
                    return;
                }
            }
        }
    });

    // Periodic DNS refresh for hostname upstreams (DDNS support).
    let dns_engine = engine.clone();
    let dns_handle = tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(30));
        tick.tick().await;
        loop {
            tick.tick().await;
            dns_engine.refresh_dns().await;
        }
    });

    // Periodic upstream probe sample reporter.
    let probe_tx = tx.clone();
    let probe_engine = engine.clone();
    let probe_handle = tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(60));
        tick.tick().await;
        loop {
            tick.tick().await;
            let samples = {
                let mut buf = probe_engine.probe_buf.lock().await;
                std::mem::take(&mut *buf)
            };
            for sample in samples {
                let msg = NodeMessage {
                    payload: Some(relay_proto::v1::node_message::Payload::UpstreamProbe(
                        sample,
                    )),
                };
                if probe_tx.send(msg).await.is_err() {
                    return;
                }
            }
        }
    });

    // 连接事件上报（ConnectEvent / DisconnectEvent），5s 排空一次缓冲。
    let conn_tx = tx.clone();
    let conn_engine = engine.clone();
    let conn_handle = tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(5));
        tick.tick().await;
        loop {
            tick.tick().await;
            let events = {
                let mut buf = conn_engine.conn_event_buf.lock().await;
                std::mem::take(&mut *buf)
            };
            for msg in events {
                if conn_tx.send(msg).await.is_err() {
                    return;
                }
            }
        }
    });

    let result: Result<SessionExit> = async {
        loop {
            tokio::select! {
                _ = cert_session.reconnect.notified() => {
                    return Ok(SessionExit::CertRotated);
                }
                msg = inbound.message() => {
                    let Some(msg) = msg? else { return Ok(SessionExit::Eof); };
                    let Some(payload) = msg.payload else { continue };
                    match payload {
                        MasterPayload::Config(cfg) => {
                            let version = cfg.version;
                            tracing::info!(version, forwards = cfg.forwards.len(), "config received");
                            let ack = engine.apply(cfg).await;
                            let ack_msg = NodeMessage {
                                payload: Some(relay_proto::v1::node_message::Payload::Ack(ack)),
                            };
                            if tx.send(ack_msg).await.is_err() {
                                tracing::warn!("ack channel closed");
                                return Ok(SessionExit::Eof);
                            }
                            tracing::info!(version, "config applied");
                        }
                        MasterPayload::Command(cmd) => {
                            use relay_proto::v1::command::Kind as CmdKind;
                            let kind = CmdKind::try_from(cmd.kind).unwrap_or(CmdKind::Noop);
                            match kind {
                                CmdKind::UpgradeAgent => {
                                    let tx = tx.clone();
                                    tokio::spawn(async move {
                                        upgrade::handle_upgrade_command(cmd, tx).await;
                                    });
                                }
                                _ => {
                                    tracing::info!(?cmd, "command received (not implemented)");
                                }
                            }
                        }
                        MasterPayload::Probe(req) => {
                            let tx = tx.clone();
                            tokio::spawn(async move {
                                let result = run_probe(req).await;
                                let msg = NodeMessage {
                                    payload: Some(
                                        relay_proto::v1::node_message::Payload::ProbeResult(
                                            result,
                                        ),
                                    ),
                                };
                                let _ = tx.send(msg).await;
                            });
                        }
                        MasterPayload::RenewCert(resp) => {
                            if let Some(sender) = cert_session.pending.lock().await.take() {
                                let _ = sender.send(resp);
                            } else {
                                tracing::warn!("received RenewCertResponse with no pending request");
                            }
                        }
                    }
                }
            }
        }
    }
    .await;

    hb_handle.abort();
    stats_handle.abort();
    dns_handle.abort();
    probe_handle.abort();
    conn_handle.abort();
    *cert_session.outbound.lock().await = None;
    result
}

async fn run_probe(req: ProbeRequest) -> ProbeResult {
    let timeout = if req.timeout_ms == 0 {
        Duration::from_secs(5)
    } else {
        Duration::from_millis(req.timeout_ms as u64)
    };
    let started = Instant::now();
    // ProbeKind is a proto enum — i32 in generated Rust.
    let kind = relay_proto::v1::ProbeKind::try_from(req.kind)
        .unwrap_or(relay_proto::v1::ProbeKind::Connect);
    match kind {
        relay_proto::v1::ProbeKind::Connect => {
            const SAMPLES: usize = 3;
            let mut latencies: Vec<u64> = Vec::with_capacity(SAMPLES);
            let mut last_err = String::new();
            for _ in 0..SAMPLES {
                let t = Instant::now();
                match tokio::time::timeout(timeout, tokio::net::TcpStream::connect(&req.target))
                    .await
                {
                    Ok(Ok(_)) => latencies.push(t.elapsed().as_micros() as u64),
                    Ok(Err(e)) => last_err = format!("connect: {e}"),
                    Err(_) => last_err = format!("timeout after {}ms", timeout.as_millis()),
                }
            }
            if latencies.is_empty() {
                ProbeResult {
                    request_id: req.request_id,
                    ok: false,
                    latency_us: 0,
                    error: last_err,
                }
            } else {
                ProbeResult {
                    request_id: req.request_id,
                    ok: true,
                    latency_us: latencies.iter().sum::<u64>() / latencies.len() as u64,
                    error: String::new(),
                }
            }
        }
        relay_proto::v1::ProbeKind::BindTcp => {
            let parsed = req.target.parse::<std::net::SocketAddr>();
            match parsed
                .map_err(anyhow::Error::from)
                .and_then(crate::forward::bind_tcp_listener)
            {
                Ok(listener) => {
                    drop(listener);
                    ProbeResult {
                        request_id: req.request_id,
                        ok: true,
                        latency_us: started.elapsed().as_micros().min(u64::MAX as u128) as u64,
                        error: String::new(),
                    }
                }
                Err(e) => ProbeResult {
                    request_id: req.request_id,
                    ok: false,
                    latency_us: 0,
                    error: format!("bind tcp: {e}"),
                },
            }
        }
        relay_proto::v1::ProbeKind::BindTcpHold => {
            let parsed = req.target.parse::<std::net::SocketAddr>();
            match parsed
                .map_err(anyhow::Error::from)
                .and_then(crate::forward::bind_tcp_listener)
            {
                Ok(listener) => {
                    tokio::spawn(async move {
                        tokio::time::sleep(timeout).await;
                        drop(listener);
                    });
                    ProbeResult {
                        request_id: req.request_id,
                        ok: true,
                        latency_us: started.elapsed().as_micros().min(u64::MAX as u128) as u64,
                        error: String::new(),
                    }
                }
                Err(e) => ProbeResult {
                    request_id: req.request_id,
                    ok: false,
                    latency_us: 0,
                    error: format!("bind tcp: {e}"),
                },
            }
        }
        relay_proto::v1::ProbeKind::BindUdp => {
            let parsed = req.target.parse::<std::net::SocketAddr>();
            match parsed
                .map_err(anyhow::Error::from)
                .and_then(crate::forward::bind_udp_socket)
            {
                Ok(sock) => {
                    drop(sock);
                    ProbeResult {
                        request_id: req.request_id,
                        ok: true,
                        latency_us: started.elapsed().as_micros().min(u64::MAX as u128) as u64,
                        error: String::new(),
                    }
                }
                Err(e) => ProbeResult {
                    request_id: req.request_id,
                    ok: false,
                    latency_us: 0,
                    error: format!("bind udp: {e}"),
                },
            }
        }
    }
}

async fn connect_mtls(cli: &Cli) -> Result<NodeServiceClient<tonic::transport::Channel>> {
    let ca_path = cli.pki_dir.join("ca.crt");
    let cert_path = cli.pki_dir.join("node.crt");
    let key_path = cli.pki_dir.join("node.key");

    for p in [&ca_path, &cert_path, &key_path] {
        if !p.exists() {
            return Err(anyhow!(
                "missing PKI file {} — run enrollment (M4.4) first",
                p.display()
            ));
        }
    }

    let ca = std::fs::read(&ca_path).with_context(|| format!("reading {}", ca_path.display()))?;
    let cert =
        std::fs::read(&cert_path).with_context(|| format!("reading {}", cert_path.display()))?;
    let key =
        std::fs::read(&key_path).with_context(|| format!("reading {}", key_path.display()))?;

    let server_name = match &cli.master_server_name {
        Some(s) => s.clone(),
        None => master_host(&cli.master)?,
    };

    let tls = ClientTlsConfig::new()
        .ca_certificate(Certificate::from_pem(ca))
        .identity(Identity::from_pem(cert, key))
        .domain_name(server_name);

    let endpoint = Endpoint::from_shared(cli.master.clone())?
        .tls_config(tls)?
        .connect_timeout(Duration::from_secs(10))
        .http2_keep_alive_interval(Duration::from_secs(20))
        .keep_alive_timeout(Duration::from_secs(10))
        .keep_alive_while_idle(true);
    let channel = endpoint.connect().await?;
    Ok(NodeServiceClient::new(channel))
}

fn master_host(url: &str) -> Result<String> {
    // Strip scheme and trailing path/port.
    let stripped = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or(url);
    let host_port = stripped.split('/').next().unwrap_or(stripped);
    let host = host_port
        .rsplit_once(':')
        .map(|(h, _)| h)
        .unwrap_or(host_port);
    if host.is_empty() {
        return Err(anyhow!("could not derive server name from {url:?}"));
    }
    Ok(host.to_string())
}

async fn cold_start_enroll(cli: &Cli) -> Result<()> {
    let token = cli.token.as_deref().ok_or_else(|| {
        anyhow!(
            "PKI dir {} is empty and NODE_TOKEN/--token is not set",
            cli.pki_dir.display()
        )
    })?;
    let ca_b64 = cli.ca_cert_b64.as_deref().ok_or_else(|| {
        anyhow!(
            "PKI dir {} is empty and NODE_CA_CERT_B64/--ca-cert-b64 is not set",
            cli.pki_dir.display()
        )
    })?;
    let ca_pem = enroll::decode_ca_cert(ca_b64)?;

    let endpoint = match &cli.master_enroll_endpoint {
        Some(s) => s.clone(),
        None => default_enroll_endpoint(&cli.master)?,
    };

    tracing::info!(%endpoint, "cold-start: enrolling with master");
    enroll::enroll(enroll::EnrollInput {
        pki_dir: cli.pki_dir.clone(),
        node_id: cli.node_id.clone(),
        token: token.to_string(),
        master_enroll_endpoint: endpoint,
        ca_cert_pem: ca_pem,
    })
    .await
}

fn default_enroll_endpoint(master: &str) -> Result<String> {
    let host = master_host(master)?;
    Ok(format!("http://{host}:7080/api/v1/enroll"))
}

fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Parse the `notAfter` field out of a PEM-encoded X.509 certificate.
fn cert_not_after_from_pem(pem: &str) -> Result<OffsetDateTime> {
    use x509_parser::pem::parse_x509_pem;
    let (_, p) = parse_x509_pem(pem.as_bytes()).map_err(|e| anyhow!("PEM parse: {e}"))?;
    let cert = p.parse_x509().map_err(|e| anyhow!("X509 parse: {e}"))?;
    let ts = cert.validity().not_after.timestamp();
    OffsetDateTime::from_unix_timestamp(ts).map_err(|e| anyhow!("not_after timestamp: {e}"))
}

const RENEW_THRESHOLD: time::Duration = time::Duration::days(60);
const RENEW_CHECK_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);
const RENEW_RETRY_INTERVAL: Duration = Duration::from_secs(60 * 60);
const RENEW_RPC_TIMEOUT: Duration = Duration::from_secs(60);

/// Long-lived task that watches the local node cert's expiry and rotates it
/// in-band over the existing mTLS channel when less than `RENEW_THRESHOLD`
/// remains. On success it pulses `cert_session.reconnect` so the main loop
/// reopens the gRPC channel using the freshly-installed identity.
async fn cert_renewer(node_id: String, pki_dir: PathBuf, cert_session: Arc<CertSession>) {
    let crt_path = pki_dir.join("node.crt");
    let key_path = pki_dir.join("node.key");

    loop {
        let sleep_dur = match try_renew_once(&node_id, &crt_path, &key_path, &cert_session).await {
            Ok(true) => {
                cert_session.reconnect.notify_waiters();
                RENEW_CHECK_INTERVAL
            }
            Ok(false) => RENEW_CHECK_INTERVAL,
            Err(e) => {
                tracing::warn!(error = ?e, "cert renew attempt failed; will retry later");
                RENEW_RETRY_INTERVAL
            }
        };
        tokio::time::sleep(sleep_dur).await;
    }
}

/// Returns `Ok(true)` when a rotation actually happened, `Ok(false)` when the
/// cert still has plenty of life. Errors are transient and worth retrying.
async fn try_renew_once(
    node_id: &str,
    crt_path: &Path,
    key_path: &Path,
    cert_session: &Arc<CertSession>,
) -> Result<bool> {
    let pem = std::fs::read_to_string(crt_path)
        .with_context(|| format!("reading {}", crt_path.display()))?;
    let not_after = cert_not_after_from_pem(&pem)?;
    let remaining = not_after - OffsetDateTime::now_utc();
    if remaining > RENEW_THRESHOLD {
        tracing::debug!(
            remaining_days = remaining.whole_days(),
            "cert still fresh; skipping renew"
        );
        return Ok(false);
    }
    tracing::info!(
        remaining_days = remaining.whole_days(),
        "cert nearing expiry, attempting in-band renewal"
    );

    let key = rcgen::KeyPair::generate().context("generating new keypair")?;
    let key_pem = key.serialize_pem();
    let csr_pem = enroll::build_csr(node_id, &key)?;

    let outbound = cert_session
        .outbound
        .lock()
        .await
        .clone()
        .ok_or_else(|| anyhow!("no active master session — will retry"))?;

    let (resp_tx, resp_rx) = oneshot::channel();
    {
        let mut pending = cert_session.pending.lock().await;
        if pending.is_some() {
            return Err(anyhow!("another renew is already in flight"));
        }
        *pending = Some(resp_tx);
    }

    let send_res = outbound
        .send(NodeMessage {
            payload: Some(relay_proto::v1::node_message::Payload::RenewCert(
                RenewCertRequest { csr_pem },
            )),
        })
        .await;
    if send_res.is_err() {
        cert_session.pending.lock().await.take();
        return Err(anyhow!("session closed before request was sent"));
    }

    let resp: RenewCertResponse = match tokio::time::timeout(RENEW_RPC_TIMEOUT, resp_rx).await {
        Ok(Ok(r)) => r,
        Ok(Err(_)) => {
            return Err(anyhow!("session dropped while awaiting renew response"));
        }
        Err(_) => {
            cert_session.pending.lock().await.take();
            return Err(anyhow!(
                "timed out after {:?} waiting for renew response",
                RENEW_RPC_TIMEOUT
            ));
        }
    };

    if !resp.error.is_empty() {
        return Err(anyhow!("master rejected renew: {}", resp.error));
    }
    if resp.node_cert_pem.is_empty() {
        return Err(anyhow!("master returned empty cert with no error"));
    }

    // Write order: key first, then cert. Each write is atomic
    // (tempfile+rename) but the pair is not, so a crash between the two
    // writes leaves cert/key out of sync. The master still trusts the old
    // fingerprint until it sees this one's UPDATE land, so on next boot the
    // node will fail mTLS and the cert_renewer will retry — eventually
    // converging. Acceptable for a 60-day renewal window.
    enroll::write_secret(key_path, key_pem.as_bytes())
        .with_context(|| format!("writing {}", key_path.display()))?;
    enroll::write_secret(crt_path, resp.node_cert_pem.as_bytes())
        .with_context(|| format!("writing {}", crt_path.display()))?;

    tracing::info!(
        not_after_unix_ms = resp.not_after_unix_ms,
        "cert rotated; triggering reconnect with new identity"
    );
    Ok(true)
}

//! In-memory rolling series of node heartbeats and per-tunnel stats.
//!
//! Each node has a bounded ring of the last `MAX_SAMPLES` samples for
//! its heartbeat and for every tunnel it has reported on. Master never
//! persists these to disk — they're for the detail page only and are
//! lost across restarts.

use std::collections::HashMap;
use std::sync::Arc;

use serde::Serialize;
use tokio::sync::RwLock;

/// Tracks last seen monotonic counters per rule so we can compute deltas
/// for traffic accumulation into `user_quota`. Resets on master restart;
/// since node counters are also reset on node restart we tolerate brief
/// under-counting around restarts.
#[derive(Clone, Default)]
pub struct CounterDeltas {
    #[allow(clippy::type_complexity)]
    inner: Arc<RwLock<HashMap<(String, i64, u32), (u64, u64)>>>,
}

impl CounterDeltas {
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns (delta_in, delta_out) given new monotonic counter values.
    /// Keyed by (node_id, forward_id, hop_index) so multi-hop forwards
    /// can't pollute each other.
    pub async fn record(
        &self,
        node_id: &str,
        forward_id: i64,
        hop_index: u32,
        bytes_in: u64,
        bytes_out: u64,
    ) -> (u64, u64) {
        let key = (node_id.to_string(), forward_id, hop_index);
        let mut g = self.inner.write().await;
        let (last_in, last_out) = g.get(&key).copied().unwrap_or((0, 0));
        let d_in = bytes_in.saturating_sub(last_in);
        let d_out = bytes_out.saturating_sub(last_out);
        let (d_in, d_out) = if bytes_in < last_in || bytes_out < last_out {
            (bytes_in, bytes_out)
        } else {
            (d_in, d_out)
        };
        g.insert(key, (bytes_in, bytes_out));
        (d_in, d_out)
    }
}

const MAX_SAMPLES: usize = 120; // 10 min @ 5s cadence

#[derive(Debug, Clone, Serialize)]
pub struct HeartbeatSample {
    pub ts_unix_ms: i64,
    pub cpu_pct: f64,
    pub mem_used_bytes: u64,
    pub mem_total_bytes: u64,
    pub active_connections: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct TunnelSample {
    pub ts_unix_ms: i64,
    pub bytes_in: u64,
    pub bytes_out: u64,
    pub active_connections: u32,
    pub total_connections: u64,
}

#[derive(Default)]
struct NodeBuf {
    heartbeats: std::collections::VecDeque<HeartbeatSample>,
    tunnels: HashMap<String, std::collections::VecDeque<TunnelSample>>,
}

#[derive(Clone, Default)]
pub struct SeriesStore {
    inner: Arc<RwLock<HashMap<String, NodeBuf>>>,
}

#[derive(Debug, Serialize)]
pub struct NodeSeries {
    pub heartbeats: Vec<HeartbeatSample>,
    pub tunnels: HashMap<String, Vec<TunnelSample>>,
}

impl SeriesStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn push_heartbeat(&self, node_id: &str, sample: HeartbeatSample) {
        let mut g = self.inner.write().await;
        let buf = g.entry(node_id.to_string()).or_default();
        if buf.heartbeats.len() == MAX_SAMPLES {
            buf.heartbeats.pop_front();
        }
        buf.heartbeats.push_back(sample);
    }

    pub async fn push_tunnel(&self, node_id: &str, tunnel_id: &str, sample: TunnelSample) {
        let mut g = self.inner.write().await;
        let buf = g.entry(node_id.to_string()).or_default();
        let q = buf.tunnels.entry(tunnel_id.to_string()).or_default();
        if q.len() == MAX_SAMPLES {
            q.pop_front();
        }
        q.push_back(sample);
    }

    /// Sum the latest active_connections across every (node_id, forward_id:0)
    /// pair. For layered DAG entry layers with multiple nodes, this gives
    /// the aggregate active count served by all DNS-LB entry nodes.
    pub async fn latest_forward_active_many(&self, node_ids: &[String], forward_id: &str) -> u32 {
        let key = format!("{forward_id}:0");
        let stale_cutoff = chrono::Utc::now().timestamp_millis() - 30_000;
        let g = self.inner.read().await;
        let mut total: u32 = 0;
        for n in node_ids {
            if let Some(v) = g
                .get(n)
                .and_then(|buf| buf.tunnels.get(&key))
                .and_then(|q| q.back())
                .filter(|s| s.ts_unix_ms >= stale_cutoff)
                .map(|s| s.active_connections)
            {
                total = total.saturating_add(v);
            }
        }
        total
    }

    /// Returns (rx_bytes_per_sec, tx_bytes_per_sec) aggregated across all
    /// tunnels for every node, computed from the last two samples of each tunnel.
    pub async fn all_node_net_speeds(&self) -> HashMap<String, (f64, f64)> {
        let g = self.inner.read().await;
        let mut out = HashMap::new();
        for (node_id, buf) in g.iter() {
            let mut rx = 0.0f64;
            let mut tx = 0.0f64;
            for samples in buf.tunnels.values() {
                let len = samples.len();
                if len < 2 {
                    continue;
                }
                let prev = &samples[len - 2];
                let last = &samples[len - 1];
                let dt = (last.ts_unix_ms - prev.ts_unix_ms) as f64 / 1000.0;
                if dt > 0.0 {
                    rx += last.bytes_in.saturating_sub(prev.bytes_in) as f64 / dt;
                    tx += last.bytes_out.saturating_sub(prev.bytes_out) as f64 / dt;
                }
            }
            out.insert(node_id.clone(), (rx, tx));
        }
        out
    }

    pub async fn series(&self, node_id: &str) -> NodeSeries {
        let g = self.inner.read().await;
        let Some(buf) = g.get(node_id) else {
            return NodeSeries {
                heartbeats: Vec::new(),
                tunnels: HashMap::new(),
            };
        };
        NodeSeries {
            heartbeats: buf.heartbeats.iter().cloned().collect(),
            tunnels: buf
                .tunnels
                .iter()
                .map(|(k, v)| (k.clone(), v.iter().cloned().collect()))
                .collect(),
        }
    }
}

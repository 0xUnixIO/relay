//! 后台任务：订阅 stats_tx broadcast，每 60 秒将聚合流量写入 forward_stats 表。
//!
//! 每条 ForwardStatEvent 的 bytes_in/bytes_out 是节点侧单调递增计数器。
//! 此处在内存中对齐增量，按 forward_id 累加，周期性 flush 一行至数据库。
//!
//! TimescaleDB：通过 retention policy 自动删除 30 天前数据。
//! 原生 PG：每 24 小时运行一次 DELETE 兜底清理（同样保留 30 天）。

use std::collections::HashMap;

use chrono::Utc;
use tokio::sync::broadcast::error::RecvError;
use tokio::time::{interval, Duration, MissedTickBehavior};

use crate::state::AppState;

struct Accum {
    last_in: u64,
    last_out: u64,
    delta_in: u64,
    delta_out: u64,
    peak_conns: u32,
}

pub fn spawn(state: AppState) {
    tokio::spawn(run(state));
}

async fn run(state: AppState) {
    let mut rx = state.stats_tx.subscribe();

    let mut flush_tick = interval(Duration::from_secs(60));
    flush_tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
    flush_tick.tick().await; // 跳过第一次立即触发

    // 原生 PG 兜底清理：每 24 小时删一次 30 天前的数据。
    // TimescaleDB 有 retention policy，此 DELETE 在 hypertable 上幂等无害。
    let mut cleanup_tick = interval(Duration::from_secs(86400));
    cleanup_tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
    cleanup_tick.tick().await;

    let mut accum: HashMap<i64, Accum> = HashMap::new();

    loop {
        tokio::select! {
            res = rx.recv() => {
                match res {
                    Ok(evt) => {
                        let fwd_id: i64 = match evt.forward_id.parse() {
                            Ok(v) => v,
                            Err(_) => continue,
                        };
                        let entry = accum.entry(fwd_id).or_insert(Accum {
                            last_in:    evt.bytes_in,
                            last_out:   evt.bytes_out,
                            delta_in:   0,
                            delta_out:  0,
                            peak_conns: 0,
                        });
                        let d_in = if evt.bytes_in >= entry.last_in {
                            evt.bytes_in - entry.last_in
                        } else {
                            evt.bytes_in // 计数器归零（节点重启）
                        };
                        let d_out = if evt.bytes_out >= entry.last_out {
                            evt.bytes_out - entry.last_out
                        } else {
                            evt.bytes_out
                        };
                        entry.last_in    = evt.bytes_in;
                        entry.last_out   = evt.bytes_out;
                        entry.delta_in  += d_in;
                        entry.delta_out += d_out;
                        entry.peak_conns = entry.peak_conns.max(evt.active_connections);
                    }
                    Err(RecvError::Lagged(_)) => continue,
                    Err(RecvError::Closed)    => break,
                }
            }

            _ = flush_tick.tick() => {
                if accum.is_empty() { continue; }
                let ts = Utc::now();
                for (&fwd_id, acc) in &accum {
                    if acc.delta_in == 0 && acc.delta_out == 0 { continue; }
                    let _ = sqlx::query(
                        "INSERT INTO forward_stats (ts, forward_id, bytes_in, bytes_out, peak_conns) \
                         VALUES ($1, $2, $3, $4, $5)",
                    )
                    .bind(ts)
                    .bind(fwd_id)
                    .bind(acc.delta_in  as i64)
                    .bind(acc.delta_out as i64)
                    .bind(acc.peak_conns as i32)
                    .execute(&state.db)
                    .await;
                }
                for acc in accum.values_mut() {
                    acc.delta_in   = 0;
                    acc.delta_out  = 0;
                    acc.peak_conns = 0;
                }
            }

            _ = cleanup_tick.tick() => {
                let _ = sqlx::query(
                    "DELETE FROM forward_stats WHERE ts < now() - INTERVAL '30 days'",
                )
                .execute(&state.db)
                .await;
            }
        }
    }
}

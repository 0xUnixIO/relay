//! 后台任务：订阅 stats_tx broadcast，每 60 秒将聚合流量写入持久化表。
//!
//! - forward_stats：按 forward_id 聚合，用于转发历史趋势和用户结算
//! - node_stats：按 node_id 聚合，记录每个节点真实处理的字节数（不做多跳叠加）
//!
//! 每条 ForwardStatEvent 的 bytes_in/bytes_out 是节点侧单调递增计数器。
//! 此处在内存中对齐增量，按对应维度累加，周期性 flush 至数据库。
//! 时序数据的清理统一由 maintenance 模块负责。

use std::collections::HashMap;

use chrono::Utc;
use tokio::sync::broadcast::error::RecvError;
use tokio::time::{interval, Duration, MissedTickBehavior};

use crate::state::AppState;

struct FwdAccum {
    last_in: u64,
    last_out: u64,
    delta_in: u64,
    delta_out: u64,
    peak_conns: u32,
}

struct NodeAccum {
    last_in: u64,
    last_out: u64,
    delta_in: u64,
    delta_out: u64,
}

pub fn spawn(state: AppState) {
    tokio::spawn(run(state));
}

async fn run(state: AppState) {
    let mut rx = state.stats_tx.subscribe();

    let mut flush_tick = interval(Duration::from_secs(60));
    flush_tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
    flush_tick.tick().await; // 跳过第一次立即触发

    // key: (forward_id, node_id) — 每个入口节点独立追踪计数器，避免多节点互相覆盖 last_in
    // 只收录 hop_index=0 的事件，forward_stats 语义 = 入口流量（客户端实际用量）
    let mut fwd_accum: HashMap<(i64, String), FwdAccum> = HashMap::new();
    // key: (node_id, forward_id) — 同一节点的同一转发合并，避免多 hop 上报被重复计入
    let mut node_accum: HashMap<(String, i64), NodeAccum> = HashMap::new();

    loop {
        tokio::select! {
            res = rx.recv() => {
                match res {
                    Ok(evt) => {
                        let fwd_id: i64 = match evt.forward_id.parse() {
                            Ok(v) => v,
                            Err(_) => continue,
                        };

                        // --- forward 维度（流量趋势用，只统计入口跳）---
                        if evt.hop_index == 0 {
                            let fe = fwd_accum
                                .entry((fwd_id, evt.node_id.clone()))
                                .or_insert(FwdAccum {
                                    last_in:    evt.bytes_in,
                                    last_out:   evt.bytes_out,
                                    delta_in:   0,
                                    delta_out:  0,
                                    peak_conns: 0,
                                });
                            let d_in = if evt.bytes_in >= fe.last_in {
                                evt.bytes_in - fe.last_in
                            } else {
                                evt.bytes_in
                            };
                            let d_out = if evt.bytes_out >= fe.last_out {
                                evt.bytes_out - fe.last_out
                            } else {
                                evt.bytes_out
                            };
                            fe.last_in    = evt.bytes_in;
                            fe.last_out   = evt.bytes_out;
                            fe.delta_in  += d_in;
                            fe.delta_out += d_out;
                            fe.peak_conns = fe.peak_conns.max(evt.active_connections);
                        }

                        // --- node 维度（节点实际吞吐，监控用）---
                        let ne = node_accum
                            .entry((evt.node_id.clone(), fwd_id))
                            .or_insert(NodeAccum {
                                last_in:  evt.bytes_in,
                                last_out: evt.bytes_out,
                                delta_in:  0,
                                delta_out: 0,
                            });
                        let nd_in = if evt.bytes_in >= ne.last_in {
                            evt.bytes_in - ne.last_in
                        } else {
                            evt.bytes_in
                        };
                        let nd_out = if evt.bytes_out >= ne.last_out {
                            evt.bytes_out - ne.last_out
                        } else {
                            evt.bytes_out
                        };
                        ne.last_in    = evt.bytes_in;
                        ne.last_out   = evt.bytes_out;
                        ne.delta_in  += nd_in;
                        ne.delta_out += nd_out;
                    }
                    Err(RecvError::Lagged(_)) => continue,
                    Err(RecvError::Closed)    => break,
                }
            }

            _ = flush_tick.tick() => {
                let ts = Utc::now();

                // flush forward_stats（入口口径，多入口节点分别写行，查询时 SUM）
                for ((fwd_id, _node_id), acc) in &fwd_accum {
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
                for acc in fwd_accum.values_mut() {
                    acc.delta_in   = 0;
                    acc.delta_out  = 0;
                    acc.peak_conns = 0;
                }

                // flush node_stats（按 node_id 聚合，忽略 forward_id 维度）
                let mut node_totals: HashMap<String, (i64, i64)> = HashMap::new();
                for ((node_id, _fwd_id), acc) in &node_accum {
                    if acc.delta_in == 0 && acc.delta_out == 0 { continue; }
                    let e = node_totals.entry(node_id.clone()).or_insert((0, 0));
                    e.0 += acc.delta_in  as i64;
                    e.1 += acc.delta_out as i64;
                }
                for (node_id, (b_in, b_out)) in &node_totals {
                    let _ = sqlx::query(
                        "INSERT INTO node_stats (ts, node_id, bytes_in, bytes_out) \
                         VALUES ($1, $2, $3, $4)",
                    )
                    .bind(ts)
                    .bind(node_id)
                    .bind(b_in)
                    .bind(b_out)
                    .execute(&state.db)
                    .await;
                }
                for acc in node_accum.values_mut() {
                    acc.delta_in  = 0;
                    acc.delta_out = 0;
                }
            }
        }
    }
}

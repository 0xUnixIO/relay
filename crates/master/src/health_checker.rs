//! Upstream 健康状态管理。
//!
//! 基于节点上报的探测样本（UpstreamProbeSample）被动感知 upstream 健康状态：
//! - 连续失败 >= EJECT_THRESHOLD 次 → 驱逐（ejected_at 非 NULL）
//! - 任意一次成功 → 清除驱逐，重置计数
//!
//! 状态变化时调用 push_for_forward，向所有承载该 forward 的节点推送最新配置。
//! ConfigUpdate 中的 ejected_upstream_addrs 由 registry::build_config_snapshot 填充。
//! 节点收到后，路由时跳过被驱逐的 upstream，但仍探测它们以感知恢复。

use sqlx::PgPool;
use tracing;

use crate::registry::NodeRegistry;

/// 连续探测失败多少次后驱逐 upstream。
const EJECT_THRESHOLD: i32 = 3;

/// 处理一次探测样本，更新健康状态。
/// 返回 true 表示驱逐状态发生了变化（需要调用方推送配置）。
pub async fn process_probe(
    db: &PgPool,
    forward_id: i64,
    upstream_addr: &str,
    success: bool,
) -> bool {
    if success {
        // 探测成功：若当前处于驱逐状态则清除，并重置失败计数
        let changed: i64 = sqlx::query_scalar(
            "WITH upd AS (
               UPDATE forward_upstream_health
                  SET ejected_at = NULL, consecutive_failures = 0
                WHERE forward_id = $1 AND upstream_addr = $2 AND ejected_at IS NOT NULL
               RETURNING 1
             ) SELECT count(*) FROM upd",
        )
        .bind(forward_id)
        .bind(upstream_addr)
        .fetch_one(db)
        .await
        .unwrap_or(0);

        if changed > 0 {
            tracing::info!(
                forward_id,
                upstream_addr,
                "upstream recovered, removed from ejected list"
            );
            return true;
        }

        // 未驱逐时顺带清零失败计数（忽略错误）
        let _ = sqlx::query(
            "UPDATE forward_upstream_health
                SET consecutive_failures = 0
              WHERE forward_id = $1 AND upstream_addr = $2",
        )
        .bind(forward_id)
        .bind(upstream_addr)
        .execute(db)
        .await;

        false
    } else {
        // 探测失败：累积计数，达到阈值后驱逐
        let row: Option<(i32, bool)> = sqlx::query_as(
            "INSERT INTO forward_upstream_health (forward_id, upstream_addr, consecutive_failures)
             VALUES ($1, $2, 1)
             ON CONFLICT (forward_id, upstream_addr) DO UPDATE
                SET consecutive_failures = forward_upstream_health.consecutive_failures + 1
             RETURNING consecutive_failures, ejected_at IS NOT NULL",
        )
        .bind(forward_id)
        .bind(upstream_addr)
        .fetch_optional(db)
        .await
        .unwrap_or(None);

        if let Some((failures, already_ejected)) = row {
            if !already_ejected && failures >= EJECT_THRESHOLD {
                let _ = sqlx::query(
                    "UPDATE forward_upstream_health
                        SET ejected_at = now()
                      WHERE forward_id = $1 AND upstream_addr = $2",
                )
                .bind(forward_id)
                .bind(upstream_addr)
                .execute(db)
                .await;

                tracing::warn!(
                    forward_id,
                    upstream_addr,
                    failures,
                    "upstream ejected after consecutive probe failures"
                );
                return true;
            }
        }

        false
    }
}

/// 向承载该 forward 的所有在线节点推送最新配置。
pub async fn push_for_forward(db: &PgPool, registry: &NodeRegistry, forward_id: i64) {
    let node_ids: Vec<(String,)> =
        match sqlx::query_as("SELECT DISTINCT node_id FROM forward_ports WHERE forward_id = $1")
            .bind(forward_id)
            .fetch_all(db)
            .await
        {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(forward_id, error = %e, "health_checker: fetch nodes failed");
                return;
            }
        };

    for (nid,) in &node_ids {
        registry.push_config(db, nid).await;
    }
}

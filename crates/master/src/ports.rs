//! Port allocation for forwards across all hops in a tunnel.
//!
//! Topology supports layered DAG: each `hop_index` may contain multiple
//! nodes (DNS-LB / fan-in). Within one layer, **all nodes share a single
//! `listen_port`** (enforced by the deferrable trigger
//! `forward_ports_check_port_invariant_trg` from migration 0007).
//!
//! Allocation primitives:
//! - `try_insert_layer_port`: try to claim a specific port on all (node,
//!   protocol) combinations of one layer, atomically rolling back on any
//!   conflict.
//! - `pick_layer_port`: random retry then guaranteed SELECT-fallback to
//!   find a port available on every node and every protocol of the layer.
//! - `allocate_forward_ports`: top-level entry that walks the tunnel
//!   layer-by-layer.
//! - `reallocate_layer_port`: atomic same-layer port replacement, used by
//!   the bind-probe repair path.
//!
//! Hot path uses random INSERT-with-conflict, falling back to a guaranteed
//! generate_series scan when all retries are unlucky.

use std::collections::{BTreeMap, BTreeSet};

use axum::http::StatusCode;
use rand::Rng;
use sqlx::{Postgres, Transaction};

use crate::http::ApiError;

/// Try to INSERT `port` for every (node_id, protocol) of one layer. Returns
/// `true` if every row was inserted (no per-node conflict). On any conflict
/// rolls back the partial inserts made for **this candidate port**, so the
/// caller can retry with another port.
async fn try_insert_layer_port(
    tx: &mut Transaction<'_, Postgres>,
    forward_id: i64,
    hop_index: i32,
    node_ids: &[String],
    protocols: &[&str],
    port: i32,
) -> Result<bool, ApiError> {
    // Track inserted (node_id, proto) pairs for rollback on partial failure.
    let mut inserted: Vec<(String, &str)> = Vec::with_capacity(node_ids.len() * protocols.len());

    for node_id in node_ids {
        for proto in protocols {
            let row: Option<(i32,)> = sqlx::query_as(
                "INSERT INTO forward_ports (forward_id, hop_index, node_id, protocol, listen_port)
                 VALUES ($1, $2, $3, $4, $5)
                 ON CONFLICT (node_id, protocol, listen_port) DO NOTHING
                 RETURNING listen_port",
            )
            .bind(forward_id)
            .bind(hop_index)
            .bind(node_id)
            .bind(*proto)
            .bind(port)
            .fetch_optional(&mut **tx)
            .await
            .map_err(ApiError::from)?;
            if row.is_none() {
                // Conflict — undo every row we inserted for this candidate.
                for (done_node, done_proto) in &inserted {
                    sqlx::query(
                        "DELETE FROM forward_ports
                          WHERE forward_id = $1 AND hop_index = $2
                            AND node_id = $3 AND protocol = $4 AND listen_port = $5",
                    )
                    .bind(forward_id)
                    .bind(hop_index)
                    .bind(done_node)
                    .bind(*done_proto)
                    .bind(port)
                    .execute(&mut **tx)
                    .await
                    .map_err(ApiError::from)?;
                }
                return Ok(false);
            }
            inserted.push((node_id.clone(), *proto));
        }
    }
    Ok(true)
}

/// Pick one random port that is free for every (node, protocol) pair in
/// the layer, falling back to a deterministic scan over the common range.
/// Caller MUST have inserted all the layer's rows committed/blocked under
/// advisory lock for the result to remain valid past return.
pub async fn pick_free_port<'e, E>(
    executor: E,
    node_id: &str,
    low: i32,
    high: i32,
    protocols: &[&str],
) -> sqlx::Result<Option<i32>>
where
    E: sqlx::Executor<'e, Database = sqlx::Postgres>,
{
    let row: Option<(i32,)> = if protocols.is_empty() {
        sqlx::query_as(
            "SELECT n FROM generate_series($1::INT, $2::INT) AS gs(n)
              WHERE NOT EXISTS (
                  SELECT 1 FROM forward_ports WHERE node_id = $3 AND listen_port = gs.n
              )
              ORDER BY random() LIMIT 1",
        )
        .bind(low)
        .bind(high)
        .bind(node_id)
        .fetch_optional(executor)
        .await?
    } else {
        sqlx::query_as(
            "SELECT n FROM generate_series($1::INT, $2::INT) AS gs(n)
              WHERE NOT EXISTS (
                  SELECT 1 FROM forward_ports
                   WHERE node_id = $3 AND protocol = ANY($4::text[])
                     AND listen_port = gs.n
              )
              ORDER BY random() LIMIT 1",
        )
        .bind(low)
        .bind(high)
        .bind(node_id)
        .bind(protocols)
        .fetch_optional(executor)
        .await?
    };
    Ok(row.map(|(p,)| p))
}

/// Find one port in `[low, high]` that is free on **every** (node, proto)
/// combination of the layer.
async fn pick_layer_free_port(
    tx: &mut Transaction<'_, Postgres>,
    node_ids: &[String],
    protocols: &[&str],
    low: i32,
    high: i32,
) -> Result<Option<i32>, ApiError> {
    let row: Option<(i32,)> = sqlx::query_as(
        "SELECT n FROM generate_series($1::INT, $2::INT) AS gs(n)
          WHERE NOT EXISTS (
              SELECT 1 FROM forward_ports fp
               WHERE fp.node_id = ANY($3::text[])
                 AND fp.protocol = ANY($4::text[])
                 AND fp.listen_port = gs.n
          )
          ORDER BY random() LIMIT 1",
    )
    .bind(low)
    .bind(high)
    .bind(node_ids)
    .bind(protocols)
    .fetch_optional(&mut **tx)
    .await
    .map_err(ApiError::from)?;
    Ok(row.map(|(p,)| p))
}

/// Look up which (forward, protocol) is sitting on `(any-node-of-layer, port)`,
/// used for human-friendly conflict errors.
async fn find_layer_port_owner(
    tx: &mut Transaction<'_, Postgres>,
    node_ids: &[String],
    port: i32,
    protocols: &[&str],
) -> Result<Option<(i64, String, String)>, ApiError> {
    let row: Option<(i64, String, String)> = sqlx::query_as(
        "SELECT forward_id, node_id, protocol FROM forward_ports
          WHERE node_id = ANY($1::text[])
            AND listen_port = $2 AND protocol = ANY($3::text[])
          LIMIT 1",
    )
    .bind(node_ids)
    .bind(port)
    .bind(protocols)
    .fetch_optional(&mut **tx)
    .await
    .map_err(ApiError::from)?;
    Ok(row)
}

/// Compute the intersected port range covering every node in the layer.
/// Errors if any node is missing or the intersection is empty.
async fn layer_common_range(
    tx: &mut Transaction<'_, Postgres>,
    node_ids: &[String],
) -> Result<(i32, i32), ApiError> {
    let row: Option<(Option<i32>, Option<i32>)> = sqlx::query_as(
        "SELECT MAX(port_range_start), MIN(port_range_end)
           FROM nodes WHERE id = ANY($1::text[])",
    )
    .bind(node_ids)
    .fetch_optional(&mut **tx)
    .await
    .map_err(ApiError::from)?;
    let (lo, hi) = row.and_then(|(a, b)| Some((a?, b?))).ok_or_else(|| {
        ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("无法解析层节点 {node_ids:?} 的端口范围"),
        )
    })?;
    if lo > hi {
        return Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!(
                "层 {node_ids:?} 内节点端口范围无交集（公共范围 {lo}-{hi}）；请调整节点 port_range 后重试"
            ),
        ));
    }
    Ok((lo, hi))
}

/// Allocate the same listen_port for every (node, protocol) of one layer.
/// Random retry first, then guaranteed scan. If `requested` is provided
/// (entry layer only), the function fails closed when that port is not
/// uniformly free across the layer.
async fn allocate_layer_port(
    tx: &mut Transaction<'_, Postgres>,
    forward_id: i64,
    hop_index: i32,
    node_ids: &[String],
    protocols: &[&str],
    requested: Option<i32>,
) -> Result<i32, ApiError> {
    let (lo, hi) = layer_common_range(tx, node_ids).await?;
    let proto_label = protocols.join("+").to_uppercase();
    let layer_label = format!("第 {} 层", hop_index + 1);

    if let Some(port) = requested.filter(|p| *p > 0) {
        if !(1..=65535).contains(&port) {
            return Err(ApiError::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                format!("入口端口超出范围：{port}"),
            ));
        }
        if !(lo..=hi).contains(&port) {
            return Err(ApiError::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                format!("入口端口 {port} 不在 {layer_label} 节点的公共范围 {lo}-{hi} 内"),
            ));
        }
        if !try_insert_layer_port(tx, forward_id, hop_index, node_ids, protocols, port).await? {
            let owner = find_layer_port_owner(tx, node_ids, port, protocols).await?;
            let msg = match owner {
                Some((fid, n, proto)) => format!(
                    "入口端口 {port} 在节点 {n} 的 {} 协议已被转发 {fid} 占用",
                    proto.to_uppercase()
                ),
                None => format!("端口 {port} 在 {layer_label} 上不可用"),
            };
            return Err(ApiError::new(StatusCode::CONFLICT, msg));
        }
        return Ok(port);
    }

    // Fast path: random INSERT, expected ~1 try when range sparse.
    for _ in 0..8 {
        let p = rand::thread_rng().gen_range(lo..=hi);
        if try_insert_layer_port(tx, forward_id, hop_index, node_ids, protocols, p).await? {
            return Ok(p);
        }
    }

    // Guaranteed fallback under advisory lock.
    let p = pick_layer_free_port(tx, node_ids, protocols, lo, hi)
        .await?
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                format!(
                    "{layer_label}（{} 个节点）在公共范围 {lo}-{hi} 内没有同时可用于 {proto_label} 的端口；请扩大节点端口范围或减少同层节点",
                    node_ids.len()
                ),
            )
        })?;
    if !try_insert_layer_port(tx, forward_id, hop_index, node_ids, protocols, p).await? {
        return Err(ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            format!("多次重试后仍无法在 {layer_label} 分配公共端口"),
        ));
    }
    Ok(p)
}

/// Allocate listen ports on every layer of `tunnel_id` for `forward_id`.
///
/// `protocols`: tunnel's protocols set; each (layer, port) gets one row
/// per (node × protocol) on the same `listen_port` (enforced by trigger).
///
/// `requested_in_port`:
///   * `Some(p)` (1..=65535): require `p` on the entry layer for every
///     entry node × protocol combination, fail closed otherwise.
///   * `None` or `Some(0)`: pick a port available across all entry-layer
///     (node × protocol) pairs.
pub async fn allocate_forward_ports(
    tx: &mut Transaction<'_, Postgres>,
    forward_id: i64,
    tunnel_id: i64,
    protocols: &[&str],
    requested_in_port: Option<i32>,
) -> Result<(), ApiError> {
    if protocols.is_empty() {
        return Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "隧道未配置协议",
        ));
    }

    // 1. Read tunnel hops grouped by hop_index (a layer may contain >1 nodes).
    let rows: Vec<(i32, String)> = sqlx::query_as(
        "SELECT hop_index, node_id FROM tunnel_hops
          WHERE tunnel_id = $1 ORDER BY hop_index, node_id",
    )
    .bind(tunnel_id)
    .fetch_all(&mut **tx)
    .await
    .map_err(ApiError::from)?;
    if rows.is_empty() {
        return Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "隧道未配置跳点",
        ));
    }
    let mut layers: BTreeMap<i32, Vec<String>> = BTreeMap::new();
    for (idx, n) in rows {
        layers.entry(idx).or_default().push(n);
    }

    // 2. Acquire per-node advisory lock in sorted order. Released at tx end.
    let unique_nodes: BTreeSet<&str> = layers
        .values()
        .flat_map(|v| v.iter().map(String::as_str))
        .collect();
    for n in &unique_nodes {
        sqlx::query("SELECT pg_advisory_xact_lock(hashtext($1))")
            .bind(*n)
            .execute(&mut **tx)
            .await
            .map_err(ApiError::from)?;
    }

    // 3. For each layer, allocate one shared port across all nodes/protocols.
    for (hop_index, node_ids) in &layers {
        let want = if *hop_index == 0 {
            requested_in_port.filter(|p| *p > 0)
        } else {
            None
        };
        allocate_layer_port(tx, forward_id, *hop_index, node_ids, protocols, want).await?;
    }

    Ok(())
}

/// Find every node that needs a fresh config push because its forward
/// configurations might depend on `node_id`'s server_ips. That is, every
/// node that is the *predecessor* of a hop hosted by `node_id`.
pub async fn predecessors_of(db: &sqlx::PgPool, node_id: &str) -> sqlx::Result<Vec<String>> {
    let rows: Vec<(String,)> = sqlx::query_as(
        "SELECT DISTINCT prev.node_id
           FROM forward_ports h
           JOIN forward_ports prev
             ON prev.forward_id = h.forward_id
            AND prev.hop_index = h.hop_index - 1
            AND prev.protocol = h.protocol
          WHERE h.node_id = $1",
    )
    .bind(node_id)
    .fetch_all(db)
    .await?;
    Ok(rows.into_iter().map(|(n,)| n).collect())
}

/// Atomically replace the listen_port of an entire layer.
///
/// Used by the bind-probe repair path. Same-layer nodes share one port
/// (deferred trigger), so reallocating just one node would violate the
/// invariant — instead we DELETE the whole layer's rows for `protocols`
/// and pick a fresh common port.
///
/// Returns the new port. Caller is responsible for triggering config push
/// to every node in this layer **and** every node in the previous layer
/// (whose `upstream_addrs` reference this layer's port).
pub async fn reallocate_layer_port(
    db: &sqlx::PgPool,
    forward_id: i64,
    hop_index: i32,
    protocols: &[&str],
    requested: Option<i32>,
) -> Result<i32, ApiError> {
    if protocols.is_empty() {
        return Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "未指定协议",
        ));
    }

    let mut tx = db.begin().await.map_err(ApiError::from)?;

    // Fetch all nodes in this layer for the given forward.
    let node_ids: Vec<String> = {
        let rows: Vec<(String,)> = sqlx::query_as(
            "SELECT DISTINCT node_id FROM forward_ports
              WHERE forward_id = $1 AND hop_index = $2
              ORDER BY node_id",
        )
        .bind(forward_id)
        .bind(hop_index)
        .fetch_all(&mut *tx)
        .await
        .map_err(ApiError::from)?;
        rows.into_iter().map(|(n,)| n).collect()
    };
    if node_ids.is_empty() {
        return Err(ApiError::new(
            StatusCode::NOT_FOUND,
            format!("forward {forward_id} 第 {} 层无节点", hop_index + 1),
        ));
    }

    // Lock all layer nodes in sorted order.
    for n in &node_ids {
        sqlx::query("SELECT pg_advisory_xact_lock(hashtext($1))")
            .bind(n)
            .execute(&mut *tx)
            .await
            .map_err(ApiError::from)?;
    }

    // DELETE the layer's rows for these protocols (whole layer at once so
    // the deferred same-port trigger stays satisfied at commit).
    sqlx::query(
        "DELETE FROM forward_ports
          WHERE forward_id = $1 AND hop_index = $2
            AND protocol = ANY($3::text[])",
    )
    .bind(forward_id)
    .bind(hop_index)
    .bind(protocols)
    .execute(&mut *tx)
    .await
    .map_err(ApiError::from)?;

    let new_port = allocate_layer_port(
        &mut tx, forward_id, hop_index, &node_ids, protocols, requested,
    )
    .await?;

    // If this is the entry layer, sync forwards.in_port.
    if hop_index == 0 {
        sqlx::query("UPDATE forwards SET in_port = $2, updated_at = now() WHERE id = $1")
            .bind(forward_id)
            .bind(new_port)
            .execute(&mut *tx)
            .await
            .map_err(ApiError::from)?;
    }

    tx.commit().await.map_err(ApiError::from)?;
    Ok(new_port)
}

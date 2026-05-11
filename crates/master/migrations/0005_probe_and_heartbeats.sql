-- forward_probe_samples — 上游探测记录（保留 30 天）
CREATE TABLE forward_probe_samples (
    id            BIGSERIAL PRIMARY KEY,
    forward_id    BIGINT NOT NULL,
    upstream_addr TEXT NOT NULL,
    latency_us    BIGINT,          -- NULL = 失败/超时
    probed_at     TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX forward_probe_samples_lookup
    ON forward_probe_samples (forward_id, upstream_addr, probed_at DESC);

-- ------------------------------------------------------------------
-- node_heartbeats — 节点心跳时序记录（每次心跳一行，约 5s 间隔）
-- ------------------------------------------------------------------
CREATE TABLE node_heartbeats (
    ts                  TIMESTAMPTZ      NOT NULL,
    node_id             TEXT             NOT NULL,
    cpu_pct             DOUBLE PRECISION NOT NULL,
    mem_used_bytes      BIGINT           NOT NULL,
    mem_total_bytes     BIGINT           NOT NULL,
    active_connections  INT              NOT NULL
);

-- TimescaleDB 可用时启用 hypertable + 压缩 + 保留策略。
-- 不可用时静默降级为普通索引，清理由 Rust 后台任务负责。
DO $$
DECLARE tsdb_ok boolean := false;
BEGIN
    BEGIN
        CREATE EXTENSION IF NOT EXISTS timescaledb CASCADE;
        PERFORM create_hypertable('node_heartbeats', 'ts');
        ALTER TABLE node_heartbeats SET (
            timescaledb.compress,
            timescaledb.compress_segmentby = 'node_id',
            timescaledb.compress_orderby   = 'ts DESC'
        );
        PERFORM add_compression_policy('node_heartbeats', INTERVAL '1 day');
        PERFORM add_retention_policy('node_heartbeats', INTERVAL '30 days');
        tsdb_ok := true;
    EXCEPTION WHEN OTHERS THEN
        NULL;
    END;

    IF NOT tsdb_ok THEN
        CREATE INDEX node_heartbeats_node_ts_idx ON node_heartbeats (node_id, ts DESC);
    END IF;
END
$$;

-- ------------------------------------------------------------------
-- node_availability → TimescaleDB hypertable（原普通表升级）
-- 保留 90 天，压缩已满 1 天的 chunk。
-- ------------------------------------------------------------------
DO $$
BEGIN
    BEGIN
        CREATE EXTENSION IF NOT EXISTS timescaledb CASCADE;
        PERFORM create_hypertable('node_availability', 'recorded_at',
                                  migrate_data => true, if_not_exists => true);
        ALTER TABLE node_availability SET (
            timescaledb.compress,
            timescaledb.compress_segmentby = 'node_id',
            timescaledb.compress_orderby   = 'recorded_at DESC'
        );
        PERFORM add_compression_policy('node_availability', INTERVAL '1 day');
        PERFORM add_retention_policy('node_availability', INTERVAL '90 days');
    EXCEPTION WHEN OTHERS THEN
        NULL;
    END;
END
$$;

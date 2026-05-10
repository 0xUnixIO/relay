-- ------------------------------------------------------------------
-- forward_stats — 每分钟聚合的转发流量记录
-- ------------------------------------------------------------------
CREATE TABLE forward_stats (
    ts          TIMESTAMPTZ NOT NULL,
    forward_id  BIGINT      NOT NULL REFERENCES forwards(id) ON DELETE CASCADE,
    bytes_in    BIGINT      NOT NULL DEFAULT 0,
    bytes_out   BIGINT      NOT NULL DEFAULT 0,
    peak_conns  INT         NOT NULL DEFAULT 0
);

-- TimescaleDB 可用时启用 hypertable + 压缩 + 保留策略。
-- 不可用（原生 PG 未安装扩展）时静默降级：建普通索引，清理由 Rust 后台任务负责。
DO $$
DECLARE tsdb_ok boolean := false;
BEGIN
    BEGIN
        CREATE EXTENSION IF NOT EXISTS timescaledb CASCADE;
        PERFORM create_hypertable('forward_stats', 'ts');
        ALTER TABLE forward_stats SET (
            timescaledb.compress,
            timescaledb.compress_segmentby = 'forward_id',
            timescaledb.compress_orderby   = 'ts DESC'
        );
        PERFORM add_compression_policy('forward_stats', INTERVAL '1 day');
        PERFORM add_retention_policy('forward_stats', INTERVAL '30 days');
        tsdb_ok := true;
    EXCEPTION WHEN OTHERS THEN
        NULL;
    END;

    IF NOT tsdb_ok THEN
        CREATE INDEX forward_stats_fwd_ts_idx ON forward_stats (forward_id, ts DESC);
    END IF;
END
$$;

-- ------------------------------------------------------------------
-- node_stats — 每分钟聚合的节点实际流量（每个节点自己处理的字节数）
-- 与 forward_stats 不同：这里不做多跳叠加，只记录单节点真实吞吐。
-- ------------------------------------------------------------------
CREATE TABLE node_stats (
    ts        TIMESTAMPTZ NOT NULL,
    node_id   TEXT        NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
    bytes_in  BIGINT      NOT NULL DEFAULT 0,
    bytes_out BIGINT      NOT NULL DEFAULT 0
);

DO $$
DECLARE tsdb_ok boolean := false;
BEGIN
    BEGIN
        PERFORM create_hypertable('node_stats', 'ts');
        ALTER TABLE node_stats SET (
            timescaledb.compress,
            timescaledb.compress_segmentby = 'node_id',
            timescaledb.compress_orderby   = 'ts DESC'
        );
        PERFORM add_compression_policy('node_stats', INTERVAL '1 day');
        PERFORM add_retention_policy('node_stats', INTERVAL '30 days');
        tsdb_ok := true;
    EXCEPTION WHEN OTHERS THEN
        NULL;
    END;

    IF NOT tsdb_ok THEN
        CREATE INDEX node_stats_node_ts_idx ON node_stats (node_id, ts DESC);
    END IF;
END
$$;

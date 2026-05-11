-- ------------------------------------------------------------------
-- connection_logs — 客户端连接审计日志
-- 由入口节点（hop_index=0）上报，记录每次 TCP 连接的建立与断开。
-- ------------------------------------------------------------------
CREATE TABLE connection_logs (
    id              BIGINT       PRIMARY KEY,
    forward_id      BIGINT       NOT NULL REFERENCES forwards(id) ON DELETE CASCADE,
    node_id         TEXT         NOT NULL,
    conn_id         TEXT         NOT NULL UNIQUE,
    client_ip       TEXT         NOT NULL,
    connected_at    TIMESTAMPTZ  NOT NULL,
    disconnected_at TIMESTAMPTZ,
    bytes_in        BIGINT       NOT NULL DEFAULT 0,
    bytes_out       BIGINT       NOT NULL DEFAULT 0
);

CREATE INDEX conn_logs_fwd_time_idx ON connection_logs (forward_id, connected_at DESC);
CREATE INDEX conn_logs_ip_idx       ON connection_logs (client_ip);
CREATE INDEX conn_logs_active_idx   ON connection_logs (forward_id) WHERE disconnected_at IS NULL;

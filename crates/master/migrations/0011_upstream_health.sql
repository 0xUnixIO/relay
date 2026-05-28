-- upstream 健康状态表：master 基于节点探测上报，追踪每个 (forward, upstream) 的连续失败次数。
-- 达到驱逐阈值后写入 ejected_at；节点探测恢复时清除（ejected_at = NULL）。
-- forward 删除时级联清理。
CREATE TABLE forward_upstream_health (
    forward_id          BIGINT      NOT NULL REFERENCES forwards(id) ON DELETE CASCADE,
    upstream_addr       TEXT        NOT NULL,
    consecutive_failures INT        NOT NULL DEFAULT 0,
    ejected_at          TIMESTAMPTZ,          -- NULL = healthy；非 NULL = 已驱逐
    PRIMARY KEY (forward_id, upstream_addr)
);

-- 快速查询某 forward 下当前被驱逐的 upstream
CREATE INDEX forward_upstream_health_ejected_idx
    ON forward_upstream_health (forward_id)
    WHERE ejected_at IS NOT NULL;

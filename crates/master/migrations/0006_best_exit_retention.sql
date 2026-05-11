-- -----------------------------------------------------------------------
-- 0006: 最优出口策略 + 监控数据保留配置
-- -----------------------------------------------------------------------

-- lb_strategy: 新增 'best' 变体（master 负责将其翻译为 primary_backup 下发节点）
ALTER TABLE forwards
    DROP CONSTRAINT IF EXISTS forwards_lb_strategy_check;

ALTER TABLE forwards
    ADD CONSTRAINT forwards_lb_strategy_check
        CHECK (lb_strategy IN ('round_robin', 'random', 'least_latency', 'primary_backup', 'best'));

-- system_config: 监控数据保留天数（作用于 forward_stats / node_heartbeats）
ALTER TABLE system_config
    ADD COLUMN IF NOT EXISTS monitor_retention_days INT NOT NULL DEFAULT 30
        CHECK (monitor_retention_days BETWEEN 1 AND 3650);

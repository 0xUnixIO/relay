-- 记录每个 (forward, node) 跳最近一次被节点成功应用配置的时间
ALTER TABLE forward_ports ADD COLUMN last_applied_at TIMESTAMPTZ DEFAULT NULL;

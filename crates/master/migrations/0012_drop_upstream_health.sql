-- 退役 master 自动驱逐：剔除不通上游已下沉到 node 端数据面被动熔断，
-- forward_upstream_health 表（连同其驱逐状态/失败计数）已无任何读写方，删除之。
-- DROP TABLE 会一并清理 forward_upstream_health_ejected_idx 索引。
DROP TABLE IF EXISTS forward_upstream_health;

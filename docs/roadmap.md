# Relay 功能路线图

> 基于竞品 FLVX（flux-panel 深度重构版）对比分析，梳理 relay 当前功能差距与待实现特性。

---

## 当前优势（relay 领先的方向）

| 特性 | relay | FLVX |
|------|-------|------|
| 转发引擎性能 | Rust 零拷贝 `splice(2)` | Go GOST 修改版 |
| 节点安全通信 | gRPC mTLS 双向流 | HTTP + WebSocket 明文指令 |
| 内置 PKI / CA | 自动签发、自动续期 | 无 |
| 部署复杂度（节点） | 静态单二进制 + systemd | Docker + Go 运行时 |
| 远程节点升级 | 全程托管 + 失败自动回滚 | 不支持 |
| 多跳 DAG 拓扑 | 任意跳、任意 Hop 均可负载均衡 | 基础多跳 |

---

## 功能差距清单

### P0 — 核心功能补全

#### 1. 最优出口自动选择（Best Exit Selection）

**现状**：relay 有延迟探测（Probe 页面），但结果仅供展示，不影响转发路由。  
**FLVX 的做法**：在隧道上引入 `best` 策略，后台持续采样各出口节点的延迟 + 丢包，基于评分函数（`score = latency * 1000 / (100 - loss_pct)`）自动切换到最优出口，且不中断已有连接（debounce + cooldown 机制）。

**需要实现**：
- 在隧道上增加 `strategy: best` 选项（现有：`round_robin` / `random` / `sticky`）
- 后台评分循环：消费现有 probe 数据，对 `best` 隧道的所有出口候选打分
- 最优出口变更时，仅更新新建连接的路由，已有连接保持不动
- Web UI：隧道表格显示当前生效的出口节点（`当前出口: [节点名] 延迟 25ms`）

---

#### 2. 单 IP 粒度限速与连接限制（Per-IP Limits）

**现状**：relay 有全局 `speed_limit_kbps`（令牌桶，作用于整条转发）和 `max_conn`（作用于整条转发）。  
**FLVX 的做法**：使用 GOST 的 `$$` 语法，为每个客户端 IP 建立独立的令牌桶和连接计数器，彼此隔离互不影响。

**需要实现**：
- 在转发上新增字段：`ip_max_conn`（单 IP 最大连接数，0 = 不限）和 `ip_speed_kbps`（单 IP 带宽上限，0 = 不限）
- relay-node 侧：在 tokio 监听层接入 per-IP 计数器（可复用现有令牌桶抽象）
- 协议：gRPC `ForwardConfig` 消息中新增对应字段
- Web UI：转发创建/编辑时展示「每客户端限制」折叠区域

---

#### 3. 最大连接数 UI 改进

**现状**：`max_conn` 字段已存在于用户-隧道配额中，但 FLVX 同时支持 **用户级全局上限** 和 **单条转发独立上限**，两者取覆盖逻辑（转发级 > 用户级）。  
**需要实现**：
- 在用户管理界面增加「全局最大连接数」字段（跨该用户所有转发生效）
- 单条转发保留独立 `max_conn` 覆盖项
- 后端实现覆盖优先级逻辑

---

### P1 — 运营支撑

#### 4. 监控数据保留策略（Monitoring Data Retention）

**现状**：relay 以 5 秒为粒度采样转发流量数据，但无清理策略，数据库会持续增长。  
**需要实现**：
- 系统配置页新增「监控数据保留天数」（默认 7 天，范围 1–3650）
- 后台定时清理超出保留期的采样记录（`forward_stats`、`probe_results`、`node_heartbeats` 表）
- 配置页面展示当前数据库存储占用（各表行数 + 预估大小）

---

#### 5. 系统公告（Announcement System）

**现状**：无。  
**FLVX 的做法**：Admin 在配置页编写 Markdown 公告，附带 `update_time`；用户首次访问或公告更新后自动弹出 Modal 展示（本地 `localStorage` 记录已读时间戳）。

**需要实现**：
- 数据库：`system_announcements` 表（`content TEXT`、`enabled BOOL`、`updated_at`）
- API：`GET /api/v1/announcement`（所有用户）、`PUT /api/v1/announcement`（admin）
- Web UI：
  - Admin 配置页：Markdown 编辑器 + 启用/禁用开关
  - 用户端：登录后检测公告，首次或有更新时弹出 Modal

---

#### 6. 面板分享（Panel Sharing）

**现状**：无。用户仅能使用管理员分配的隧道，无法将自己管理的资源共享给其他实例或第三方。  
**FLVX 的做法**：「面板分享」允许将节点/隧道以 Token 方式开放给另一个 FLVX 实例对接（panel-to-panel federation），用于多运营商合作场景。

**需要实现**：
- 分享 Token 管理（生成、吊销、有效期）
- 被分享方通过 Token + API 拉取可用隧道列表并创建转发
- Web UI：分享配置页 + Token 展示

---

### P2 — 配置灵活性

#### 7. 绑定本地地址 / 指定远端地址（Local/Remote Addr Config）

**现状**：入口节点默认绑定 `0.0.0.0`，出口地址仅支持 `IP:Port` 字符串。  
**FLVX 的做法**：可指定入口节点监听特定 IP（如只绑 `10.0.0.1`）；出口可指定从哪个本地 IP 出去（对多网卡节点有用）。

**需要实现**：
- 隧道配置中为每个 Hop 增加可选的 `bind_addr` 字段
- 转发配置中增加可选的 `dial_addr`（出口方向的源 IP）
- gRPC 协议字段同步更新

---

#### 8. 批量操作（Batch Operations）

**现状**：转发的启停、配置变更均为逐条操作。  
**需要实现**：
- 转发列表支持多选 + 批量启用/停用
- 可选：批量修改限速、批量迁移隧道

---

### P3 — 商业化（可选）

#### 9. 白标定制（White Label）

允许管理员自定义 Web UI 的 Logo、系统名称、主题色，用于对外运营场景。

- 系统配置中增加「品牌设置」区块
- 前端读取配置动态替换 Logo 和标题

#### 10. 许可证管理（License System）

**现状**：relay 以 AGPL-3.0 开源，当前无商业授权体系。  
如计划提供商业版 / 托管版，可考虑接入 Keygen（https://keygen.sh）实现：
- 节点数量上限
- 用户数量上限
- 功能开关（白标、高级限速等）

---

## 竞品技术路线差异

| 维度 | relay 路线 | FLVX 路线 |
|------|-----------|----------|
| 转发引擎 | Rust 原生（零拷贝、静态链接） | 修改版 GOST（Go，动态配置友好）|
| 控制面通信 | gRPC mTLS 双向流 | WebSocket + HTTP |
| 动态配置下发 | gRPC 推送 `ForwardConfig` | WebSocket 指令（`AddLimiters` 等）|
| 数据库 | PostgreSQL（必须） | SQLite（默认）/ PostgreSQL |
| 部署方式 | systemd native binary | Docker Compose 优先 |

---

## 近期开发优先级建议

```
P0（本版本）:   最大连接数 UI 改进 → 监控数据清理策略
P1（下版本）:   单IP粒度限速 → 最优出口自动选择
P2（未来版本）: 系统公告 → 面板分享 → 批量操作
P3（按需）:     白标定制 → 许可证管理
```

---

*更新日期：2026-05-11 | 基于 FLVX v3.x 功能对比*

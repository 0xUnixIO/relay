# Relay 用户手册

> **relay** 是一个基于主控/节点架构的多服务器端口转发管理系统，支持 TCP/UDP 转发、多跳级联、流量配额管理和 Web 可视化控制台。

---

## 目录

1. [系统简介](#1-系统简介)
2. [核心概念](#2-核心概念)
3. [快速开始](#3-快速开始)
4. [安装部署](#4-安装部署)
5. [Web 控制台使用](#5-web-控制台使用)
6. [节点管理](#6-节点管理)
7. [隧道管理](#7-隧道管理)
8. [转发管理](#8-转发管理)
9. [用户管理](#9-用户管理)
10. [高级功能](#10-高级功能)
11. [配置参考](#11-配置参考)
12. [维护与运维](#12-维护与运维)
13. [常见问题](#13-常见问题)

---

## 1. 系统简介

### 1.1 功能概述

Relay 用于统一管理分布在多台服务器上的端口转发规则：

- 在中央控制台创建和管理转发规则，无需逐台登录服务器
- 支持 TCP 和 UDP 协议的端口转发
- 支持 A → B → C 多跳级联转发
- 提供流量统计、连接数监控、节点健康监控
- 基于 mTLS 双向证书认证，通信全程加密

### 1.2 架构组成

```
浏览器 / API 客户端
        │
        ▼ HTTP :7080
┌───────────────────┐
│   relay-master  │  控制面（Master）
│  ─────────────── │
│  REST API         │
│  gRPC Server      │◄──── PostgreSQL 数据库
│  内置 CA / PKI    │
└───────────────────┘
      │ gRPC :7443 (mTLS)
      ▼
┌────────────────────┐     ┌────────────────────┐
│  relay-node A    │     │  relay-node B     │
│  ──────────────── │     │  ──────────────────│
│  转发引擎 (TCP/UDP)│────►│  转发引擎 (TCP/UDP) │
└────────────────────┘     └────────────────────┘
```

- **Master**：控制面，负责配置管理、证书签发、推送配置给节点
- **Node**：转发代理，接受 Master 下发的配置并执行实际的端口转发
- **数据库**：PostgreSQL，存储所有配置和统计数据

---

## 2. 核心概念

### 2.1 节点（Node）

节点是运行 `relay-node` 的服务器，每个节点有：

- 唯一的节点 ID
- 可监听的端口范围（如 20000-30000）
- 由 Master CA 签发的 mTLS 客户端证书
- 实时上报的健康状态（CPU、内存、活跃连接数）

### 2.2 隧道（Tunnel）

隧道是转发路径的**模板**，由管理员创建。一个隧道定义：

- 使用哪些节点、按什么顺序构成转发链路（Hop 跳）
- 协议（TCP / UDP）
- 是否启用

```
隧道示例：节点A → 节点B → 目标服务
  Hop 0: 节点A（入口，对外暴露端口）
  Hop 1: 节点B（中转）
  出口：目标服务的 IP:Port
```

隧道本身不占用端口，只是路径模板。

### 2.3 转发（Forward）

转发是隧道的**实际运行实例**，由用户基于已分配的隧道创建：

- 指定出口地址（目标 IP:Port）
- 系统自动为每个 Hop 分配端口
- 受流量配额约束

### 2.4 用户与配额

系统支持三种角色：

| 角色 | 权限 |
|------|------|
| `admin` | 全部操作，包括创建节点、隧道、管理用户 |
| `operator` | 管理转发和分配给自己的隧道 |
| `viewer` | 只读，查看所有资源状态 |

管理员可为用户分配特定隧道的使用配额：
- **流量配额**：每月可用流量上限（字节）
- **连接数限制**：单条转发最大并发连接数

---

## 3. 快速开始

### 3.1 本地开发环境（5 分钟）

**前置依赖**：Rust ≥1.80、Bun ≥1.1、PostgreSQL ≥14、Docker（可选）

```bash
# 1. 克隆仓库
git clone https://github.com/0xUnixIO/relay.git
cd relay

# 2. 复制环境配置
cp .env.example .env

# 3. 启动 PostgreSQL（如无本地 PG，用 Docker）
docker-compose -f deploy/docker-compose.postgres.yml up -d

# 4. 一键启动所有服务
make dev
```

`make dev` 会同时启动 Master、Node 和前端开发服务器。

- Web 控制台：http://localhost:5173
- 默认管理员账号：`admin` / `admin`（首次启动自动创建）

### 3.2 生产环境（快速安装）

```bash
# 在控制面服务器上安装 Master
bash <(curl -fsSL https://raw.githubusercontent.com/0xUnixIO/relay/main/install.sh)
```

Node 的安装命令由 Web UI 在添加节点后自动生成，直接复制到目标主机执行即可。

---

## 4. 安装部署

### 4.1 安装 Master

**系统要求**：Linux x86\_64 或 aarch64，PostgreSQL 14+

```bash
bash <(curl -fsSL https://raw.githubusercontent.com/0xUnixIO/relay/main/install.sh)
```

安装脚本会交互式询问：

| 问题 | 说明 |
|------|------|
| 公网地址或域名 | 用于 TLS 证书 SAN，节点通过此地址连接 |
| 是否安装 PostgreSQL | 若已有 PG 服务可跳过 |
| 数据库连接信息 | 或让脚本自动创建 |
| Web 域名（可选） | 如提供，脚本可自动配置 Caddy + Let's Encrypt |

安装完成后，Master 以 systemd 服务运行：

```bash
systemctl status relay-master
journalctl -u relay-master -f   # 查看实时日志
```

**固定版本安装**：

```bash
bash install.sh --version v0.1.3
bash install.sh --prerelease   # 安装最新预发布版本
```

### 4.2 数据库初始化（手动）

若不使用安装脚本自动配置数据库：

```bash
# 用 PostgreSQL 超级用户创建专用角色和数据库
relay-master db init \
  --admin-url postgres://postgres:pw@localhost/postgres \
  --password <relay-db-password> \
  --database relay

# 之后手动跑迁移（serve 时也会自动执行）
relay-master db migrate
```

### 4.3 安装 Node

1. 登录 Web 控制台 → **节点** → **添加节点**
2. 填写名称、可用端口范围
3. 创建后弹窗自动显示安装命令，点击**复制命令**
4. 在节点服务器上执行该命令

Node 首次启动时自动完成 **Enrollment**（证书申请）：
1. 用 `NODE_TOKEN` POST `http://<master>:7080/api/v1/enroll`（或反代 HTTPS URL）
2. 本地生成密钥对，提交 CSR
3. Master 原子消费 token、签发客户端证书，写入 `NODE_PKI_DIR`
4. 此后通过 :7443 mTLS 与 Master 建立长连接

**Node systemd 管理**：

```bash
systemctl status relay-node
journalctl -u relay-node -f
```

### 4.4 卸载

```bash
# 卸载 Master（脚本会询问是否清除数据）
bash install.sh --uninstall

# 卸载 Node
bash install-node.sh --uninstall
```

---

## 5. Web 控制台使用

### 5.1 登录

访问 `http://<master-host>:7080`（或配置的 Web 域名）。

首次登录使用安装时设置的管理员账号。修改密码：右上角用户菜单 → **个人设置**。

### 5.2 控制台布局

```
┌─────────────────────────────────────────────────────┐
│  Relay          节点 | 隧道 | 转发 | 用户    [用户名▼] │
├──────────┬──────────────────────────────────────────┤
│          │                                          │
│  侧边栏  │            主内容区                       │
│  （导航）│                                          │
└──────────┴──────────────────────────────────────────┘
```

**主要页面**：

| 页面 | 功能 |
|------|------|
| 仪表板 | 系统概览：节点状态、活跃转发数、近期流量图表 |
| 节点 | 节点列表、健康状态、延迟探测 |
| 节点详情 | 单节点实时监控、连接数 Sparkline |
| 隧道 | 隧道模板管理（仅 admin） |
| 转发 | 创建和管理转发实例 |
| 用户 | 用户管理、配额分配（仅 admin） |
| 个人设置 | 修改密码 |
| 系统配置 | Master 运行参数（仅 admin） |

---

## 6. 节点管理

### 6.1 查看节点列表

**节点** 页面显示所有已注册节点，包含：

- 节点 ID 和主机名
- 状态：**在线** / **离线**（超过心跳超时未上报）
- 最后在线时间
- CPU 和内存使用率
- 活跃连接数
- 已部署的 Agent 版本

### 6.2 添加节点

1. 点击**添加节点**
2. 填写表单：

   | 字段 | 说明 |
   |------|------|
   | 名称 | 服务器域名或 IP，便于识别 |
   | 可用端口范围 | 转发可分配的端口范围，如 `20000-30000` |

3. 创建后弹窗自动显示安装命令，点击**复制命令**后在对应服务器执行，弹窗会等待节点上线

### 6.3 节点详情

点击节点 ID 进入详情页，可查看：

- 实时资源使用情况（10 分钟历史 Sparkline）
- 与各节点间的延迟探测结果
- 当前承载的转发列表和各转发连接数

### 6.4 重新颁发 Enrollment Token

当节点证书丢失或需要迁移时：

1. 节点详情页 → **重新颁发 Token**
2. 在节点服务器执行新的安装命令（含 `--reenroll` 标志）

---

## 7. 隧道管理

> 隧道管理需要 **admin** 角色。

### 7.1 创建隧道

1. **隧道** 页面 → **创建隧道**
2. 配置隧道路径：

   | 字段 | 说明 |
   |------|------|
   | 名称 | 便于识别的描述性名称 |
   | 协议 | TCP 或 UDP |
   | 跳（Hop） | 按顺序添加节点，构成转发链路 |

3. **Hop 配置**：
   - **入口 Hop**（Hop 0）：用户连接的入口节点
   - **中转 Hop**（可选）：流量经过的中间节点
   - 最后一跳的出口地址在创建**转发**时指定

**示例：两跳隧道**

```
用户 → [Hop 0: 香港节点, 端口自动分配] → [Hop 1: 日本节点, 端口自动分配] → 目标服务
```

### 7.2 分配隧道给用户

1. **用户** 页面 → 点击用户 → **分配隧道**
2. 选择隧道，配置配额：

   | 配额项 | 说明 |
   |--------|------|
   | 月流量上限 | 每自然月可用总流量，0 表示不限 |
   | 最大连接数 | 单条转发的最大并发连接数，0 表示不限 |
   | 有效期 | 可选，到期后自动暂停 |

### 7.3 启用/禁用隧道

隧道可全局启用或禁用。禁用后，所有基于该隧道的转发自动暂停。

---

## 8. 转发管理

### 8.1 创建转发

1. **转发** 页面 → **创建转发**
2. 填写：

   | 字段 | 说明 |
   |------|------|
   | 隧道 | 选择已分配给你的隧道 |
   | 备注 | 可选，转发用途说明 |
   | 出口地址 | 目标服务的 `IP:Port`，如 `192.168.1.100:3306` |

3. 保存后，系统自动：
   - 为每个 Hop 分配监听端口
   - 将配置推送至相关节点
   - 节点收到配置后立即开始监听

**连接地址**即为入口节点 IP + Hop 0 分配的端口。

### 8.2 查看转发状态

转发列表显示每条转发的：

- 状态：**运行中** / **暂停**（及暂停原因）
- 入口地址（节点 IP:端口）
- 出口地址
- 实时连接数
- 本月已用 / 总流量

**暂停原因说明**：

| 原因 | 含义 | 解决方式 |
|------|------|---------|
| `quota_exceeded` | 月流量已超限 | 等待下月重置，或联系 admin 提升配额 |
| `user_disabled` | 管理员禁用了用户 | 联系 admin |
| `tunnel_disabled` | 隧道被禁用 | 联系 admin |
| `expired` | 配额有效期已过 | 联系 admin 续期 |
| `deploy_failed` | 配置推送节点失败 | 检查节点是否在线 |

### 8.3 删除转发

删除转发后，节点会立即停止相应的端口监听，已占用的端口释放回端口池。

---

## 9. 用户管理

> 需要 **admin** 角色。

### 9.1 创建用户

**用户** 页面 → **创建用户**：

| 字段 | 说明 |
|------|------|
| 用户名 | 登录名，全局唯一 |
| 密码 | 初始密码（建议要求用户登录后修改） |
| 角色 | `admin` / `operator` / `viewer` |
| 备注 | 可选，如所属部门或联系方式 |

### 9.2 管理用户配额

在用户详情页管理该用户对各隧道的使用权限：

1. **添加隧道权限**：选择隧道，设置流量和连接数配额
2. **修改配额**：点击配额项直接编辑
3. **撤销权限**：移除后，该用户的相关转发自动暂停

### 9.3 禁用/启用用户

禁用用户后，其所有转发立即暂停（原因：`user_disabled`），用户无法登录。

---

## 10. 高级功能

### 10.1 多跳级联转发

用于需要流量绕行多个节点的场景（如跨境加速、内网穿透链路）：

```
用户(中国) → [香港节点] → [日本节点] → 目标服务(日本内网)
```

配置方式：创建隧道时，按顺序添加多个 Hop：
1. Hop 0：香港节点（用户连接入口）
2. Hop 1：日本节点（中间跳）

Hop 间的端口由系统自动分配，ACL 自动配置（上一跳 IP 加入下一跳的白名单）。

### 10.2 ACL 访问控制

在入口 Hop（Hop 0）上配置 CIDR 白名单或黑名单，限制允许连接的客户端 IP：

- **白名单**：仅允许列表中的 IP/网段连接
- **黑名单**：拒绝列表中的 IP/网段

中间 Hop 的 ACL 由系统自动管理（只允许上一跳节点 IP），无需手动配置。

### 10.3 限速

在转发上配置带宽限制（`speed_limit_kbps`），系统通过令牌桶算法对每个连接的吞吐量进行限速。

### 10.4 流量统计

系统每 5 秒采样一次各转发的流量数据：

- **实时**：节点详情页的 Sparkline（10 分钟历史）
- **历史**：隧道/转发页面的月度累计图表

---

## 11. 配置参考

### 11.1 Master 环境变量

| 变量 | 默认值 | 必填 | 说明 |
|------|--------|:----:|------|
| `MASTER_DATABASE_URL` | — | ✓ | PostgreSQL DSN，如 `postgres://relay:pw@localhost/relay` |
| `MASTER_PUBLIC_ADDR` | — | ✓ | Master 公网地址/域名，用于 TLS 证书 SAN |
| `MASTER_JWT_SECRET` | `dev-only-...` | 生产必填 | JWT 签名密钥，生产环境必须修改 |
| `MASTER_HTTP_ADDR` | `0.0.0.0:7080` | — | REST API + cold-start enrollment 监听地址 |
| `MASTER_GRPC_ADDR` | `0.0.0.0:7443` | — | gRPC 监听地址（mTLS） |
| `MASTER_ENROLL_PUBLIC_URL` | 自动 fallback | — | 节点 enroll 对外 URL；有反代时填反代域名（如 `https://relay.example.com/api/v1/enroll`），留空时退化为 `http://<public_host>:<http_port>/api/v1/enroll` |
| `MASTER_PKI_DIR` | `/var/lib/relay-master/pki` | — | CA 和证书存储目录 |
| `RUST_LOG` | `info` | — | 日志级别，调试时可设为 `relay_master=debug` |

### 11.2 Node 环境变量

| 变量 | 默认值 | 必填 | 说明 |
|------|--------|:----:|------|
| `NODE_ID` | — | ✓ | 节点唯一 ID，须与 Master 注册一致 |
| `NODE_MASTER_ENDPOINT` | — | ✓ | Master gRPC 地址，如 `https://master.example.com:7443` |
| `NODE_PKI_DIR` | `/var/lib/relay-node/pki` | — | 证书存储目录 |
| `NODE_TOKEN` | — | 初次启动 | 一次性 enrollment token（用完即失效） |
| `NODE_CA_CERT_B64` | — | 初次启动 | Base64 编码的 Master CA 证书 |
| `NODE_MASTER_SERVER_NAME` | 自动推导 | — | TLS SNI override |
| `NODE_MASTER_ENROLL_ENDPOINT` | 自动推导 | — | Enrollment 端点（默认 `http://<master-host>:7080/api/v1/enroll`） |
| `RUST_LOG` | `info` | — | 日志级别 |

### 11.3 Node CLI 参数

```bash
relay-node \
  --node-id <NODE_ID> \
  --master <gRPC_ENDPOINT> \
  [--pki-dir /path/to/pki] \
  [--master-server-name <TLS_SNI>] \
  [--token <ENROLLMENT_TOKEN>] \
  [--ca-cert-b64 <BASE64_CA>] \
  [--reenroll]   # 重新 enrollment（清除旧证书）
```

### 11.4 Master CLI 子命令

```bash
relay-master serve            # 启动控制面（默认）
relay-master db init          # 初始化数据库角色和 schema
relay-master db migrate       # 执行待应用的迁移
```

---

## 12. 维护与运维

### 12.1 升级

**Master 升级**：

```bash
bash install.sh --version v0.2.0   # 升级到指定版本
```

脚本会：
1. 下载新版本二进制
2. 停止服务
3. 替换二进制
4. 自动执行数据库迁移
5. 重启服务

> ⚠️ **Breaking Change**：查看 `CHANGELOG.md` 确认是否有不兼容变更，特别注意 mTLS 证书格式变化（升级前需同步升级所有 Node）。

**Node 升级**：

```bash
bash install-node.sh --version v0.2.0
```

Node 升级不需要重新 enrollment，现有证书继续有效。

### 12.2 证书管理

Master 内置 CA 和证书会**自动续期**：

- CA 证书有效期：10 年
- Master server 证书有效期：5 年
- Node client 证书有效期：365 天
- Node 证书剩余 < 60 天时，Node 主动向 Master 申请续期（无需人工操作）

**手动重颁 Node 证书**：

1. Web UI → 节点详情 → **重新颁发 Token**
2. 在节点服务器执行：
   ```bash
   relay-node --reenroll --token <新TOKEN> --ca-cert-b64 <BASE64_CA>
   ```

### 12.3 数据库备份

```bash
# 备份
pg_dump -U relay -d relay > relay_backup_$(date +%Y%m%d).sql

# 恢复
psql -U relay -d relay < relay_backup_20260101.sql
```

### 12.4 日志查看

```bash
# Master 日志
journalctl -u relay-master -f
journalctl -u relay-master --since "1 hour ago"

# Node 日志
journalctl -u relay-node -f

# 调整日志级别（临时）
systemctl edit relay-master   # 添加 Environment=RUST_LOG=debug
systemctl restart relay-master
```

### 12.5 端口占用检查

如某转发无法启动，检查端口是否被占用：

```bash
ss -tlnp | grep <PORT>
```

---

## 13. 常见问题

### Q: 节点显示"离线"，但服务器实际在运行？

**排查步骤**：
1. 检查 Node 服务状态：`systemctl status relay-node`
2. 查看 Node 日志：`journalctl -u relay-node -n 50`
3. 确认 Node 能连接到 Master：`curl -k https://master.example.com:7443`
4. 检查防火墙是否放行了 Master 的 7443 端口

### Q: 转发创建成功，但无法连接？

**排查步骤**：
1. 确认转发状态为"运行中"（非暂停）
2. 检查入口节点是否在线
3. 在入口节点上确认端口已在监听：`ss -tlnp | grep <PORT>`
4. 检查 ACL 配置，确认客户端 IP 没有被黑名单
5. 确认出口地址（目标服务）可以从最后一个节点访问

### Q: 转发状态为"超额暂停"（quota_exceeded）？

当月流量已超限。等待下月自然重置，或联系管理员：
1. 提升该用户在该隧道上的流量配额
2. 或手动重置月流量（Web UI → 用户 → 配额管理 → 重置）

### Q: 节点 enrollment 失败？

常见原因：
- Token 已被使用（一次性 token，消费后失效）→ 在 Web UI 重新生成
- `NODE_CA_CERT_B64` 格式错误 → 重新从 Web UI 复制安装命令
- 节点无法连接 Master 的 7080 端口（cold-start enrollment）→ 检查防火墙

### Q: 如何查看某条转发的流量统计？

转发列表 → 点击转发名称 → 详情页中可查看历史流量趋势图。

### Q: 多个节点在同一台服务器上可以吗？

不建议。每个 Node 实例需要唯一的 `NODE_ID` 和独立的 `NODE_PKI_DIR`，且端口范围不能重叠。

### Q: 如何迁移 Master 到新服务器？

1. 备份数据库和 PKI 目录（`/var/lib/relay-master/pki`）
2. 在新服务器安装 Master，使用相同数据库或恢复备份
3. 将 PKI 目录复制到新服务器（保持相同路径）
4. 更新 `MASTER_PUBLIC_ADDR` 为新服务器地址
5. 更新 DNS，将旧地址指向新服务器
6. 节点证书中绑定的是 Master CA，只要 CA 不变，无需重新 enrollment

---

*如需帮助，请提交 Issue：[https://github.com/0xUnixIO/relay/issues](https://github.com/0xUnixIO/relay/issues)*

# relay

[![Build](https://github.com/0xUnixIO/relay/actions/workflows/release.yml/badge.svg)](https://github.com/0xUnixIO/relay/actions/workflows/release.yml)
[![License: AGPL-3.0](https://img.shields.io/badge/License-AGPL--3.0-blue.svg)](./LICENSE)
[![Release](https://img.shields.io/github/v/release/0xUnixIO/relay)](https://github.com/0xUnixIO/relay/releases)

**relay** 是一个面向多服务器场景的端口转发管理平台，采用「主控 / 节点」架构，支持多跳级联、用户套餐管理与 Web 控制台，适合个人搭建或商业化运营。

```
客户端  →  [入口节点]  →  [中转节点]  →  [出口节点]  →  目标服务
```

---

## 特性

- **高性能**：原生 Rust 二进制，直接运行在宿主机，无容器开销；Linux 零拷贝转发（`splice(2)`），tokio 异步 I/O，低延迟、低内存占用
- **无 Docker 依赖**：master / node 均为静态编译的单一可执行文件，node 主机只需 systemd，Docker 仅用于 master 侧的 PostgreSQL / Redis（可替换为自有数据库）
- **多跳级联**：Layered DAG 拓扑，任意跳可配多台节点负载均衡，TCP + UDP 双协议
- **mTLS 安全**：主控 ↔ 节点全链路双向 TLS，内置 CA，证书自动续期
- **用户套餐**：多用户 + 角色，套餐批量绑定隧道，流量 / 限速 / 到期配额，超额自动暂停
- **一键部署**：单条命令安装 master（内置 PostgreSQL + Redis，可选 Caddy HTTPS），Web UI 生成节点安装命令
- **远程升级**：Admin 在 Web UI 发起节点升级，全程托管，失败自动回滚
- **实时监控**：流量 sparkline、延迟探测、活跃连接数

---

## 架构

```
                        ┌──────────────────┐
                        │   Web (React)    │
                        └────────┬─────────┘
                                 │ REST
  ┌────────────┐        ┌────────▼─────────┐        ┌──────────────┐
  │ PostgreSQL │◄──────►│  master (Rust)   │◄──────►│ Redis（可选）│
  └────────────┘        └────────┬─────────┘        └──────────────┘
                                 │ gRPC（mTLS 双向流）
               ┌─────────────────┼─────────────────┐
          ┌────▼────┐       ┌────▼────┐       ┌────▼────┐
          │  node   │       │  node   │  ...  │  node   │
          │ TCP/UDP │       │ TCP/UDP │       │ TCP/UDP │
          │  转发   │       │  转发   │       │  转发   │
          └─────────┘       └─────────┘       └─────────┘
```

| 层     | 技术                                               |
|--------|----------------------------------------------------|
| 后端   | Rust（tokio · tonic · axum · sqlx + PostgreSQL）   |
| 通信   | gRPC mTLS 双向流（master ↔ node）                  |
| 前端   | Bun · React · TypeScript · shadcn/ui · Tailwind v4 |
| 存储   | PostgreSQL（主数据）· Redis（可选缓存）             |

---

## 快速开始

### 1. 安装 master

```bash
bash <(curl -fsSL https://raw.githubusercontent.com/0xUnixIO/relay/main/install.sh)
```

交互向导会引导完成：PostgreSQL → Redis（可选）→ 公网地址确认 → Web 域名 + HTTPS（可选）。全程约 2 分钟。

> PostgreSQL / Redis 支持三种方式：**Docker**（推荐，所有发行版）、**apt 原生安装**（仅 Debian/Ubuntu）、或填入已有实例的连接串。非 apt 发行版请提前自行安装数据库，或选择 Docker 方式。

安装完成后：
- Web 控制台：`http://<your-ip>:7080`（或 HTTPS 域名）
- 首次访问自动引导创建 admin 账户

### 2. 添加节点

在 Web 控制台 **Nodes → New Node**，填写节点 ID 后复制弹窗给出的一键安装命令，粘贴到目标主机执行即可。节点上线后在控制台可见，可立即用于隧道配置。

### 3. 创建隧道与转发

1. **Tunnels → New Tunnel**：选择入口/中转/出口节点，配置协议
2. **用户管理**：为用户分配隧道（流量/限速/到期）
3. 用户登录后在 **Forwards** 创建转发，指定目标地址和端口
4. 复制入口地址给客户端使用

---

## 安装要求

**master 主机**
- Linux x86_64 或 aarch64
- systemd
- Docker + docker compose v2（推荐）或自有 PostgreSQL / Redis 实例；原生 apt 安装仅支持 Debian/Ubuntu
- 开放端口：`7080`（HTTP + 节点注册）、`7443`（gRPC）

> relay master 本身是原生 Rust 二进制，以 systemd 服务运行在宿主机上，不跑在容器里。

**node 主机**
- Linux x86_64 或 aarch64
- systemd（无需 Docker）
- 能访问 master 的 `7080`（cold-start enrollment）和 `7443`（gRPC mTLS）端口

---

## 文档

- [部署、升级、卸载](./docs/deployment.md)
- [本地开发](./docs/development.md)

---

## 贡献

欢迎 Issue 和 PR。提交前请确保：

```bash
cargo fmt
cargo clippy -- -D warnings
cargo test
cd web && bun run typecheck && bun run build
```

---

## License

[AGPL-3.0](./LICENSE) © 2026 0xUnixIO

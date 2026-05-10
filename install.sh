#!/usr/bin/env bash
# Install or upgrade relay-master from a GitHub release.
#
# Quick start (interactive, no sudo on the command line — script will
# self-elevate via sudo):
#   bash <(curl -fsSL https://raw.githubusercontent.com/0xUnixIO/relay/main/install.sh)
#
# Pin a version:
#   bash <(curl -fsSL .../install.sh) --version v0.1.2
#
# Flags:
#   --version <tag>     pin a specific release tag (default: latest)
#   --repo <owner/name> override the GitHub repo (default: 0xUnixIO/relay)
#   --no-start          install but don't enable/start the service
#   --update            non-interactive upgrade (skips the menu, keeps config)
#   --uninstall         stop service, remove binary + unit. Then asks
#                       interactively whether to also wipe env / PKI /
#                       Postgres data (default: keep).
#
# Interactive setup (default when stdin is a TTY and the env file has
# placeholders) will:
#   1. ask for MASTER_PUBLIC_ADDR
#   2. offer to bring up Postgres via the bundled docker compose file
#      (skipped if docker isn't available — you'll be told what to run)
#   3. auto-generate the relay db password and JWT secret with `openssl rand`
#   4. run `relay-master db init` to create the role + database
#   5. write /etc/relay-master/relay-master.env with real values
#   6. enable + start the systemd service

set -euo pipefail

REPO="0xUnixIO/relay"
VERSION="latest"
INCLUDE_PRERELEASE=0
START=1
UNINSTALL=0
MODE=""  # install | update | uninstall | show-config

log()  { printf '\033[1;34m==>\033[0m %s\n' "$*"; }
warn() { printf '\033[1;33m!!\033[0m %s\n' "$*" >&2; }
die()  { printf '\033[1;31mxx\033[0m %s\n' "$*" >&2; exit 1; }

check_ports() {
  local -a ports=(7080 7443)
  local -a busy=()
  for p in "${ports[@]}"; do
    local pid
    # `|| true` 防止干净服务器上 grep 找不到匹配（退 1）配合 pipefail+set -e 静默杀脚本
    pid=$(ss -lntpH "sport = :$p" 2>/dev/null | grep -oE 'pid=[0-9]+' | head -1 | cut -d= -f2 || true)
    [[ -z "$pid" ]] && continue
    local exe
    exe=$(readlink -f "/proc/$pid/exe" 2>/dev/null || true)
    # 如果是自身服务占用，后面会 stop，不算冲突
    [[ "$exe" == *"$BIN_NAME"* ]] && continue
    busy+=("$p (pid=$pid, ${exe##*/})")
  done
  if [[ ${#busy[@]} -gt 0 ]]; then
    die "以下端口被其他进程占用，请先释放后重试：${busy[*]}"
  fi
  log "端口检查通过：${ports[*]}"
}

show_menu() {
  echo
  printf '\033[1;34m╔══════════════════════════════════════════════════════════╗\033[0m\n'
  printf '\033[1;34m║              relay-master 管理脚本                     ║\033[0m\n'
  printf '\033[1;34m╚══════════════════════════════════════════════════════════╝\033[0m\n'
  echo
  echo "  1. 全新安装"
  echo "  2. 更新（升级二进制，保留配置数据）"
  echo "  3. 卸载"
  echo "  4. 查看当前配置"
  echo "  5. 退出"
  echo
  local choice
  read -r -p "请选择 [1-5]: " choice
  case "$choice" in
    1) MODE="install" ;;
    2) MODE="update" ;;
    3) MODE="uninstall"; UNINSTALL=1 ;;
    4) MODE="show-config" ;;
    5) exit 0 ;;
    *) warn "无效选项，请重新选择"; show_menu ;;
  esac
}

show_config() {
  if [[ ! -f "$ENV_FILE" ]]; then
    warn "配置文件不存在：$ENV_FILE（尚未安装？）"
    exit 1
  fi
  echo
  log "当前配置 ($ENV_FILE):"
  echo
  while IFS= read -r line; do
    [[ "$line" =~ ^#.*$ || -z "$line" ]] && continue
    printf '  %s\n' "$line"
  done < "$ENV_FILE"
  echo
  if systemctl is-active --quiet "$UNIT_NAME" 2>/dev/null; then
    log "服务状态：$UNIT_NAME 正在运行"
  else
    warn "服务状态：$UNIT_NAME 未运行"
  fi

  # 读取公网地址，提取第一个（逗号分隔时取首个）
  local pub_addr
  pub_addr="$(grep -E '^MASTER_PUBLIC_ADDR=' "$ENV_FILE" | head -1 | cut -d= -f2-)"
  pub_addr="${pub_addr%%,*}"
  local http_addr
  http_addr="$(grep -E '^MASTER_HTTP_ADDR=' "$ENV_FILE" | head -1 | cut -d= -f2-)"
  local web_port="${http_addr##*:}"
  local host="${pub_addr:-<this-host>}"

  # 若 Caddy 配置存在，从中提取域名显示 HTTPS 地址
  local caddy_domain=""
  local caddy_conf="/etc/caddy/conf.d/relay.caddyfile"
  if [[ -f "$caddy_conf" ]]; then
    caddy_domain="$(grep -vE '^\s*#|^\s*$' "$caddy_conf" | awk '/\{/{print $1; exit}')"
  fi

  echo
  printf '\033[1;32m══════════════════════════════════════════════════\033[0m\n'
  printf '\033[1;32m  relay-master 服务端点\033[0m\n'
  printf '\033[1;32m══════════════════════════════════════════════════\033[0m\n'
  echo
  if [[ -n "$caddy_domain" ]]; then
    log "Web 控制台:      https://${caddy_domain}"
  else
    log "Web 控制台:      http://${host}:${web_port:-7080}"
  fi
  log "gRPC 端口:       ${host}:7443"
  echo
  log "日志查看:        journalctl -u $UNIT_NAME -f"
  log "服务状态:        systemctl status $UNIT_NAME"
  log "重启服务:        systemctl restart $UNIT_NAME"
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --version)    VERSION="$2"; shift 2 ;;
    --prerelease) INCLUDE_PRERELEASE=1; shift ;;
    --repo)       REPO="$2"; shift 2 ;;
    --no-start)   START=0; shift ;;
    --update)     MODE="update"; shift ;;
    --uninstall)  MODE="uninstall"; UNINSTALL=1; shift ;;
    -h|--help)    sed -n '2,30p' "$0"; exit 0 ;;
    *) die "unknown flag: $1" ;;
  esac
done

BIN_NAME="relay-master"
UNIT_NAME="relay-master"
ETC_DIR="/etc/relay-master"
ENV_FILE="$ETC_DIR/relay-master.env"
COMPOSE_FILE="$ETC_DIR/docker-compose.postgres.yml"
COMPOSE_ENV_FILE="$ETC_DIR/postgres.env"
REDIS_COMPOSE_FILE="$ETC_DIR/docker-compose.redis.yml"
REDIS_ENV_FILE="$ETC_DIR/redis.env"

[[ $EUID -eq 0 ]] || {
  # Re-exec under sudo. Persist the script to a tempfile first so that
  # sudo can read it even when we were invoked via process substitution
  # (`bash <(curl …)` → $0 = /dev/fd/63, which sudo's FD-closing default
  # would otherwise drop).
  tmp="$(mktemp /tmp/relay-install.XXXXXX.sh)"
  case "$0" in
    /dev/fd/*) cat "$0" > "$tmp" ;;
    bash|-bash|sh|-sh) die "must run as root (try: sudo bash -c \"\$(curl -fsSL …)\")" ;;
    *) cp "$0" "$tmp" ;;
  esac
  chmod +x "$tmp"
  exec sudo bash "$tmp" "$@"
}
# Clean up the tempfile produced by self-elevation on exit.
case "$0" in /tmp/relay-install.*.sh) trap 'rm -f "$0"' EXIT ;; esac

# 未通过 flag 指定操作时，弹出交互菜单
if [[ -z "$MODE" ]]; then
  if [[ -t 0 ]] || [[ -e /dev/tty ]]; then
    [[ -t 0 ]] || exec </dev/tty
    show_menu
  else
    MODE="install"
  fi
fi
[[ -z "$MODE" ]] && MODE="install"

if [[ "$MODE" == "show-config" ]]; then
  show_config
  exit 0
fi

if [[ "$UNINSTALL" -eq 1 ]]; then
  log "stopping $UNIT_NAME"
  systemctl disable --now "$UNIT_NAME" 2>/dev/null || true
  rm -f "/etc/systemd/system/${UNIT_NAME}.service" "/usr/local/bin/${BIN_NAME}"
  systemctl daemon-reload
  log "removed binary + systemd unit"

  PURGE=0
  if { [[ -t 0 ]] || [[ -e /dev/tty ]]; }; then
    [[ -t 0 ]] || exec </dev/tty
    echo
    log "保留在磁盘上的内容："
    [[ -f "$ENV_FILE" ]] && log "  - $ENV_FILE              # 数据库密码 + JWT 密钥"
    [[ -d /var/lib/relay-master/pki ]] && log "  - /var/lib/relay-master/pki/             # CA + 服务器证书"
    [[ -f "$COMPOSE_FILE" ]] && log "  - 内置 Postgres 容器 'relay-postgres' + 数据卷 'relay-pgdata'"
    [[ -f "$REDIS_COMPOSE_FILE" ]] && log "  - 内置 Redis 容器 'relay-redis' + 数据卷 'relay-redisdata'"
    echo
    read -r -p "是否一并清除以上内容（不可恢复）？[y/N] " ans
    case "${ans:-N}" in [Yy]*) PURGE=1 ;; esac
  fi

  if [[ "$PURGE" -eq 1 ]]; then
    if [[ -f "$COMPOSE_FILE" ]]; then
      log "tearing down bundled Postgres (container + volume)"
      docker compose -f "$COMPOSE_FILE" \
        ${COMPOSE_ENV_FILE:+--env-file "$COMPOSE_ENV_FILE"} down -v 2>/dev/null || true
    fi
    if [[ -f "$REDIS_COMPOSE_FILE" ]]; then
      log "tearing down bundled Redis (container + volume)"
      docker compose -f "$REDIS_COMPOSE_FILE" \
        ${REDIS_ENV_FILE:+--env-file "$REDIS_ENV_FILE"} down -v 2>/dev/null || true
    fi
    log "wiping $ETC_DIR and /var/lib/relay-master"
    rm -rf "$ETC_DIR" /var/lib/relay-master
    rm -f /etc/caddy/conf.d/relay.caddyfile
    if [[ -f /etc/caddy/Caddyfile ]] && command -v caddy >/dev/null; then
      systemctl reload caddy 2>/dev/null || true
    fi
    if id relay >/dev/null 2>&1; then
      log "removing system user 'relay'"
      userdel relay 2>/dev/null || true
    fi
    log "purged. Nothing left on disk."
  else
    log "done. Re-run with the same uninstall command and answer 'y' to wipe everything."
  fi
  exit 0
fi

OS="$(uname -s)"
ARCH="$(uname -m)"
case "$OS-$ARCH" in
  Linux-x86_64)              TARGET="x86_64-unknown-linux-gnu" ;;
  Linux-aarch64|Linux-arm64) TARGET="aarch64-unknown-linux-gnu" ;;
  *) die "unsupported platform: $OS $ARCH" ;;
esac

# minimal Debian/Ubuntu 镜像可能没装 curl/tar，先把基础工具补齐。
ensure_base_pkgs() {
  local need=()
  command -v curl       >/dev/null || need+=(curl)
  command -v tar        >/dev/null || need+=(tar)
  command -v sha256sum  >/dev/null || need+=(coreutils)
  [[ -e /etc/ssl/certs/ca-certificates.crt ]] || need+=(ca-certificates)
  [[ ${#need[@]} -eq 0 ]] && return 0
  if [[ "$EUID" -ne 0 ]] || ! command -v apt-get >/dev/null; then
    return 0  # 让后面的 command -v 检查报错给用户
  fi
  log "安装基础依赖: ${need[*]}"
  apt-get update -qq
  DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends "${need[@]}" >/dev/null
}
ensure_base_pkgs

command -v curl       >/dev/null || die "curl is required"
command -v tar        >/dev/null || die "tar is required"
command -v sha256sum  >/dev/null || die "sha256sum is required (coreutils)"
command -v systemctl  >/dev/null || die "systemd is required"

check_ports

if [[ "$VERSION" == "latest" ]]; then
  if [[ "$INCLUDE_PRERELEASE" -eq 1 ]]; then
    # fetch recent releases and pick the newest one marked prerelease:true
    VERSION="$(curl -fsSL "https://api.github.com/repos/$REPO/releases?per_page=20" \
      | python3 -c "import sys,json; rs=[r for r in json.load(sys.stdin) if r['prerelease']]; print(rs[0]['tag_name'] if rs else '')")"
    [[ -n "$VERSION" ]] || die "failed to resolve latest pre-release"
    log "latest pre-release: $VERSION"
  else
    VERSION="$(curl -fsSL "https://api.github.com/repos/$REPO/releases/latest" \
      | grep -oE '"tag_name": *"[^"]+"' | head -1 | cut -d'"' -f4)"
    [[ -n "$VERSION" ]] || die "failed to resolve latest version (rate-limited?)"
  fi
fi

ARCHIVE="relay-${VERSION}-${TARGET}.tar.gz"
BASE="https://github.com/$REPO/releases/download/$VERSION"

log "installing relay-master $VERSION for $TARGET"

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

log "downloading $ARCHIVE"
curl -fsSL "$BASE/$ARCHIVE"   -o "$TMP/$ARCHIVE"
curl -fsSL "$BASE/SHA256SUMS" -o "$TMP/SHA256SUMS"

log "verifying sha256"
( cd "$TMP" && grep " $ARCHIVE\$" SHA256SUMS | sha256sum -c - ) \
  || die "checksum mismatch"

tar -xzf "$TMP/$ARCHIVE" -C "$TMP"
DIR="$TMP/relay-${VERSION}-${TARGET}"
[[ -f "$DIR/$BIN_NAME" ]] || die "$BIN_NAME not found in archive"

RESTART=0
if systemctl is-active --quiet "$UNIT_NAME" 2>/dev/null; then
  log "stopping running $UNIT_NAME before upgrade"
  systemctl stop "$UNIT_NAME"
  RESTART=1
fi

log "installing /usr/local/bin/$BIN_NAME"
install -m 0755 "$DIR/$BIN_NAME" "/usr/local/bin/$BIN_NAME"

if ! id relay >/dev/null 2>&1; then
  log "creating system user 'relay'"
  useradd --system --no-create-home --shell /usr/sbin/nologin relay
fi

mkdir -p "$ETC_DIR"

write_placeholder_env() {
  cat >"$ENV_FILE" <<'EOF'
# relay-master configuration
MASTER_HTTP_ADDR=0.0.0.0:7080
MASTER_GRPC_ADDR=0.0.0.0:7443

# REQUIRED. Public address(es) clients connect to (DNS names and/or IPs,
# comma-separated). Used as the SAN of the master TLS server cert. Master
# refuses to start if unset. Example:
#   MASTER_PUBLIC_ADDR=master.example.com,10.0.0.1
MASTER_PUBLIC_ADDR=CHANGE_ME

# PostgreSQL connection. Provision the database first (no psql needed):
#   sudo relay-master db init \
#     --admin-url 'postgres://postgres:<superuser_pw>@127.0.0.1:5432/postgres' \
#     --password  'CHANGE_ME'
MASTER_DATABASE_URL=postgres://relay:CHANGE_ME@127.0.0.1:5432/relay

# Optional Redis cache (probe 防抖 etc.). Leave empty to disable.
#   MASTER_REDIS_URL=redis://:<password>@127.0.0.1:6379/0
MASTER_REDIS_URL=

# Used to sign admin / node JWTs. Generate with: openssl rand -hex 32
MASTER_JWT_SECRET=CHANGE_ME_TO_A_LONG_RANDOM_STRING

RUST_LOG=info,relay_master=info
EOF
  chmod 0640 "$ENV_FILE"
  chown root:relay "$ENV_FILE"
}

env_has_placeholders() {
  [[ -f "$ENV_FILE" ]] || return 0
  grep -qE '^(MASTER_DATABASE_URL=.*CHANGE_ME|MASTER_JWT_SECRET=CHANGE_ME|MASTER_PUBLIC_ADDR=CHANGE_ME)' "$ENV_FILE"
}

INTERACTIVE=0
WEB_DOMAIN=""
if env_has_placeholders && { [[ -t 0 ]] || [[ -e /dev/tty ]]; }; then
  INTERACTIVE=1
fi
# 更新模式不重新配置 env
[[ "$MODE" == "update" ]] && INTERACTIVE=0

# When stdin is a pipe (curl | sudo bash), fall back to reading from the
# controlling TTY so prompts still work.
if [[ "$INTERACTIVE" -eq 1 && ! -t 0 ]]; then
  exec </dev/tty
fi

if [[ "$INTERACTIVE" -eq 0 ]]; then
  if [[ ! -f "$ENV_FILE" ]]; then
    log "writing placeholder $ENV_FILE (run with a TTY for interactive setup)"
    write_placeholder_env
  else
    log "$ENV_FILE already exists, leaving untouched"
    # v0.1.2+ 引入了 MASTER_ENROLL_PUBLIC_URL；旧 env 缺这一项时按现状回填。
    if ! grep -qE '^MASTER_ENROLL_PUBLIC_URL=' "$ENV_FILE"; then
      _existing_pub="$(grep -E '^MASTER_PUBLIC_ADDR=' "$ENV_FILE" | head -1 | cut -d= -f2-)"
      _existing_pub="${_existing_pub%%,*}"
      _caddy_conf="/etc/caddy/conf.d/relay.caddyfile"
      _caddy_domain=""
      [[ -f "$_caddy_conf" ]] && \
        _caddy_domain="$(grep -vE '^\s*#|^\s*$' "$_caddy_conf" | awk '/\{/{print $1; exit}')"
      if [[ -n "$_caddy_domain" ]]; then
        _enroll_url="https://${_caddy_domain}/api/v1/enroll"
      elif [[ -n "$_existing_pub" ]]; then
        _enroll_url="http://${_existing_pub}:7080/api/v1/enroll"
      else
        _enroll_url=""
      fi
      if [[ -n "$_enroll_url" ]]; then
        log "回填 MASTER_ENROLL_PUBLIC_URL=$_enroll_url 到 $ENV_FILE"
        printf '\nMASTER_ENROLL_PUBLIC_URL=%s\n' "$_enroll_url" >> "$ENV_FILE"
      else
        warn "无法推断 MASTER_ENROLL_PUBLIC_URL；请手动在 $ENV_FILE 中设置该项以保证节点 enroll 可达"
      fi
    fi
  fi
else
  command -v openssl >/dev/null || die "openssl is required for interactive setup"

  log "交互式安装 — 请回答几个问题"

  RELAY_PW="$(openssl rand -hex 16)"
  JWT_SECRET="$(openssl rand -hex 32)"
  USE_SOCKET_INIT=0

  has_docker_compose() {
    command -v docker >/dev/null && docker compose version >/dev/null 2>&1
  }

  ensure_docker() {
    if ! has_docker_compose; then
      read -r -p "未检测到 docker，通过 https://get.docker.com 安装？[Y/n] " ans
      case "${ans:-Y}" in
        [Nn]*) die "请先安装 Docker 或选择原生安装" ;;
        *)
          log "正在安装 Docker（可能需要一分钟）"
          curl -fsSL https://get.docker.com | sh
          systemctl enable --now docker || true
          has_docker_compose || die "docker compose 仍不可用，请检查 Docker 安装日志"
          ;;
      esac
    fi
  }

  start_postgres_docker() {
    ensure_docker
    log "fetching docker compose file → $COMPOSE_FILE"
    curl -fsSL "https://raw.githubusercontent.com/$REPO/main/deploy/docker-compose.postgres.yml" \
         -o "$COMPOSE_FILE"
    local SUPER_PW
    if [[ ! -f "$COMPOSE_ENV_FILE" ]]; then
      SUPER_PW="$(openssl rand -hex 16)"
      printf 'POSTGRES_PASSWORD=%s\n' "$SUPER_PW" >"$COMPOSE_ENV_FILE"
      chmod 0600 "$COMPOSE_ENV_FILE"
      log "已生成 Postgres 超级用户密码"
    else
      log "$COMPOSE_ENV_FILE 已存在，复用"
      # shellcheck disable=SC1090
      source "$COMPOSE_ENV_FILE"
      SUPER_PW="$POSTGRES_PASSWORD"
    fi
    log "启动 Postgres 容器"
    ( cd "$ETC_DIR" && docker compose --env-file "$COMPOSE_ENV_FILE" -f "$COMPOSE_FILE" up -d ) >/dev/null
    log "等待 Postgres 就绪"
    for i in $(seq 1 30); do
      docker exec relay-postgres pg_isready -U postgres >/dev/null 2>&1 && break
      sleep 1
      [[ "$i" -eq 30 ]] && die "Postgres 未在 30s 内就绪"
    done
    ADMIN_URL="postgres://postgres:${SUPER_PW}@127.0.0.1:5432/postgres"
  }

  start_postgres_native() {
    log "通过 apt 安装 PostgreSQL"
    apt-get update -qq
    DEBIAN_FRONTEND=noninteractive apt-get install -y postgresql >/dev/null
    systemctl enable --now postgresql
    log "等待 PostgreSQL 就绪"
    for i in $(seq 1 15); do
      pg_isready -U postgres >/dev/null 2>&1 && break
      sleep 1
      [[ "$i" -eq 15 ]] && die "PostgreSQL 未就绪"
    done
    ADMIN_URL="postgres:///postgres?host=/var/run/postgresql"
    USE_SOCKET_INIT=1
  }

  start_redis_docker() {
    ensure_docker
    log "fetching docker compose file → $REDIS_COMPOSE_FILE"
    curl -fsSL "https://raw.githubusercontent.com/$REPO/main/deploy/docker-compose.redis.yml" \
         -o "$REDIS_COMPOSE_FILE"
    local REDIS_PW
    if [[ ! -f "$REDIS_ENV_FILE" ]]; then
      REDIS_PW="$(openssl rand -hex 16)"
      printf 'REDIS_PASSWORD=%s\n' "$REDIS_PW" >"$REDIS_ENV_FILE"
      chmod 0600 "$REDIS_ENV_FILE"
      log "已生成 Redis 密码"
    else
      log "$REDIS_ENV_FILE 已存在，复用"
      # shellcheck disable=SC1090
      source "$REDIS_ENV_FILE"
      REDIS_PW="$REDIS_PASSWORD"
    fi
    log "启动 Redis 容器"
    ( cd "$ETC_DIR" && docker compose --env-file "$REDIS_ENV_FILE" -f "$REDIS_COMPOSE_FILE" up -d ) >/dev/null
    log "等待 Redis 就绪"
    for i in $(seq 1 20); do
      docker exec relay-redis redis-cli -a "$REDIS_PW" --no-auth-warning ping 2>/dev/null \
        | grep -q PONG && break
      sleep 1
      [[ "$i" -eq 20 ]] && die "Redis 未在 20s 内就绪"
    done
    REDIS_URL="redis://:${REDIS_PW}@127.0.0.1:6379/0"
  }

  start_redis_native() {
    log "通过 apt 安装 Redis"
    apt-get update -qq
    DEBIAN_FRONTEND=noninteractive apt-get install -y redis-server >/dev/null
    local redis_conf="/etc/redis/redis.conf"
    [[ -f "$redis_conf" ]] || redis_conf="$(find /etc/redis -name '*.conf' 2>/dev/null | head -1)"
    [[ -z "$redis_conf" ]] && die "找不到 Redis 配置文件"
    local REDIS_PW
    REDIS_PW="$(openssl rand -hex 16)"
    sed -i '/^requirepass /d' "$redis_conf"
    echo "requirepass $REDIS_PW" >> "$redis_conf"
    local redis_svc="redis-server"
    systemctl list-unit-files redis.service >/dev/null 2>&1 && redis_svc="redis"
    systemctl enable "$redis_svc" >/dev/null 2>&1 || true
    systemctl restart "$redis_svc"
    log "等待 Redis 就绪"
    for i in $(seq 1 15); do
      redis-cli -a "$REDIS_PW" --no-auth-warning ping 2>/dev/null | grep -q PONG && break
      sleep 1
      [[ "$i" -eq 15 ]] && die "Redis 未就绪"
    done
    REDIS_URL="redis://:${REDIS_PW}@127.0.0.1:6379/0"
  }

  # ── Postgres ──────────────────────────────────────────────
  echo
  ADMIN_URL=""
  read -r -p "已有 Postgres？填入 DSN（postgres://USER:PW@HOST/DB），没有直接回车: " ADMIN_URL
  if [[ -z "$ADMIN_URL" ]]; then
    echo
    echo "  选择 PostgreSQL 安装方式："
    echo "    1. 原生安装（推荐，apt install postgresql，仅 Debian/Ubuntu）"
    echo "    2. Docker（所有发行版）"
    echo
    read -r -p "  请选择 [1/2，默认 1]: " pg_choice
    case "${pg_choice:-1}" in
      2) start_postgres_docker ;;
      *) start_postgres_native ;;
    esac
  fi

  log "初始化数据库（relay-master db init）"
  if [[ "$USE_SOCKET_INIT" -eq 1 ]]; then
    # 优先 runuser（util-linux 自带，最小化镜像也有），fallback 到 sudo / su
    if command -v runuser >/dev/null 2>&1; then
      runuser -u postgres -- /usr/local/bin/relay-master db init \
        --admin-url "$ADMIN_URL" \
        --password  "$RELAY_PW"
    elif command -v sudo >/dev/null 2>&1; then
      sudo -u postgres /usr/local/bin/relay-master db init \
        --admin-url "$ADMIN_URL" \
        --password  "$RELAY_PW"
    else
      su - postgres -s /bin/sh -c "/usr/local/bin/relay-master db init --admin-url '$ADMIN_URL' --password '$RELAY_PW'"
    fi
  else
    /usr/local/bin/relay-master db init \
      --admin-url "$ADMIN_URL" \
      --password  "$RELAY_PW"
  fi

  # ── Redis ──────────────────────────────────────────────────
  echo
  REDIS_URL=""
  read -r -p "已有 Redis？填入 URL（redis://:PW@HOST:PORT/0），跳过直接回车: " REDIS_URL
  if [[ -z "$REDIS_URL" ]]; then
    echo
    echo "  选择 Redis 安装方式："
    echo "    1. 原生安装（推荐，apt install redis-server，仅 Debian/Ubuntu）"
    echo "    2. Docker（所有发行版）"
    echo "    3. 跳过（不使用 Redis，probe 防抖功能退化）"
    echo
    read -r -p "  请选择 [1/2/3，默认 1]: " redis_choice
    case "${redis_choice:-1}" in
      2) start_redis_docker ;;
      3) log "跳过 Redis" ;;
      *) start_redis_native ;;
    esac
  fi

  # ── 公网地址 ────────────────────────────────────────────
  echo
  PUBLIC_ADDR=""
  detected_lan="$(hostname -I 2>/dev/null | awk '{print $1}')"
  detected_pub="$(curl -fsSL --max-time 3 https://api.ipify.org 2>/dev/null || true)"
  default_addr=""
  hint=""
  if [[ -n "$detected_pub" && -n "$detected_lan" && "$detected_pub" != "$detected_lan" ]]; then
    default_addr="$detected_pub"
    hint="（已检测：公网=$detected_pub，内网=$detected_lan）"
  elif [[ -n "$detected_pub" ]]; then
    default_addr="$detected_pub"
    hint="（已检测公网 IP）"
  elif [[ -n "$detected_lan" ]]; then
    default_addr="$detected_lan"
    hint="（已检测内网 IP）"
  fi
  while [[ -z "$PUBLIC_ADDR" ]]; do
    if [[ -n "$default_addr" ]]; then
      read -r -p "客户端连接的公网地址（域名或 IP，多个用逗号分隔）${hint} [$default_addr]: " PUBLIC_ADDR
      PUBLIC_ADDR="${PUBLIC_ADDR:-$default_addr}"
    else
      read -r -p "客户端连接的公网地址（域名或 IP，多个用逗号分隔）: " PUBLIC_ADDR
    fi
    [[ -z "$PUBLIC_ADDR" ]] && warn "value is required"
  done

  # ── Web 域名（Caddy） ────────────────────────────────────
  echo
  log "可选：通过 Caddy + Let's Encrypt 用 HTTPS 提供 Web 控制台"
  log "  （需要公网 DNS 指向本机 + 80/443 端口未被占用）"
  WEB_DOMAIN=""
  while true; do
    read -r -p "Web 控制台域名（留空则保持纯 HTTP :7080）: " WEB_DOMAIN
    [[ -z "$WEB_DOMAIN" ]] && break
    busy=""
    for p in 80 443; do
      pids=$(ss -lntpH "sport = :$p" 2>/dev/null | grep -oE 'pid=[0-9]+' | cut -d= -f2 | sort -u || true)
      for pid in $pids; do
        unit=$(systemctl status "$pid" 2>/dev/null | head -1 | grep -oE '[a-zA-Z0-9_-]+\.service' || true)
        if [[ "$unit" == "caddy.service" ]]; then
          continue  # existing Caddy will absorb our conf.d snippet
        fi
        bin=$(readlink -f "/proc/$pid/exe" 2>/dev/null || true)
        busy="$busy  - :$p held by pid=$pid (${bin:-unknown}) ${unit:+unit=$unit}\n"
      done
    done
    if [[ -n "$busy" ]]; then
      warn "ports 80/443 are already in use:"
      printf "%b" "$busy"
      warn "Caddy can't start while another process holds these ports."
      log  "可选方案：(1) 停掉占端口的服务；(2) 跳过 Caddy，自己从已有服务器反代到 127.0.0.1:7080。"
      read -r -p "换一个域名重试还是跳过 Caddy？[retry/skip] " choice
      case "${choice:-skip}" in
        [Ss]*) WEB_DOMAIN=""; break ;;
        *) continue ;;
      esac
    fi
    break
  done

  if [[ -n "$WEB_DOMAIN" ]]; then
    HTTP_BIND="127.0.0.1:7080"
  else
    HTTP_BIND="0.0.0.0:7080"
  fi

  # 节点 enroll 走的对外 URL：有 Caddy 域名 → HTTPS 反代；否则裸 HTTP :7080
  if [[ -n "$WEB_DOMAIN" ]]; then
    ENROLL_PUBLIC_URL="https://${WEB_DOMAIN}/api/v1/enroll"
  else
    # PUBLIC_ADDR 可能是逗号分隔，取第一个
    FIRST_PUBLIC="${PUBLIC_ADDR%%,*}"
    ENROLL_PUBLIC_URL="http://${FIRST_PUBLIC}:7080/api/v1/enroll"
  fi

  log "writing $ENV_FILE"
  cat >"$ENV_FILE" <<EOF
# relay-master configuration (generated by install.sh on $(date -u +%FT%TZ))
MASTER_HTTP_ADDR=$HTTP_BIND
MASTER_GRPC_ADDR=0.0.0.0:7443

# Public address(es) clients connect to (SAN of the master TLS server cert).
MASTER_PUBLIC_ADDR=$PUBLIC_ADDR

# 节点 enroll 时该走的对外 URL（被 Web UI 拼进 install-node.sh 命令）。
# 有反代时填反代 HTTPS URL（如 https://relay.example.com/api/v1/enroll），
# 无反代时填 http://<public_host>:7080/api/v1/enroll。
MASTER_ENROLL_PUBLIC_URL=$ENROLL_PUBLIC_URL

MASTER_DATABASE_URL=postgres://relay:${RELAY_PW}@127.0.0.1:5432/relay

# 可选 Redis 缓存（probe 防抖等轻量用途）。留空 → master 不连 Redis，对应功能退化为无缓存。
MASTER_REDIS_URL=$REDIS_URL

MASTER_JWT_SECRET=$JWT_SECRET

RUST_LOG=info,relay_master=info
EOF
  chmod 0640 "$ENV_FILE"
  chown root:relay "$ENV_FILE"
fi

log "installing systemd unit"
curl -fsSL "https://raw.githubusercontent.com/$REPO/main/deploy/systemd/${UNIT_NAME}.service" \
     -o "/etc/systemd/system/${UNIT_NAME}.service"

systemctl daemon-reload

setup_caddy() {
  local domain="$1"
  log "setting up Caddy reverse proxy for https://$domain → 127.0.0.1:7080"
  if ! command -v caddy >/dev/null; then
    log "installing Caddy via official apt repo"
    apt-get update -qq
    apt-get install -y debian-keyring debian-archive-keyring apt-transport-https curl gnupg >/dev/null
    curl -1sLf 'https://dl.cloudsmith.io/public/caddy/stable/gpg.key' \
      | gpg --batch --yes --dearmor -o /usr/share/keyrings/caddy-stable-archive-keyring.gpg
    curl -1sLf 'https://dl.cloudsmith.io/public/caddy/stable/debian.deb.txt' \
      > /etc/apt/sources.list.d/caddy-stable.list
    apt-get update -qq
    apt-get install -y caddy >/dev/null
  fi
  mkdir -p /etc/caddy/conf.d
  cat >/etc/caddy/conf.d/relay.caddyfile <<EOF
# Managed by relay install.sh — edit MASTER_HTTP_ADDR + this file together.
$domain {
    reverse_proxy 127.0.0.1:7080
}
EOF
  if ! grep -qF "/etc/caddy/conf.d/" /etc/caddy/Caddyfile 2>/dev/null; then
    echo "" >> /etc/caddy/Caddyfile
    echo "import /etc/caddy/conf.d/*.caddyfile" >> /etc/caddy/Caddyfile
  fi
  systemctl enable --now caddy >/dev/null 2>&1 || true
  systemctl reload caddy 2>/dev/null || systemctl restart caddy
  log "Caddy 已配置完成。请确保 80/443 端口已开放，并将 DNS A/AAAA 指向本机。"
}

if [[ -n "$WEB_DOMAIN" ]]; then
  setup_caddy "$WEB_DOMAIN"
fi

if [[ "$START" -eq 1 ]]; then
  if env_has_placeholders; then
    warn "$ENV_FILE still has placeholders. Edit it then run:"
    warn "    sudo systemctl enable --now $UNIT_NAME"
  else
    log "enabling + starting $UNIT_NAME"
    systemctl enable --now "$UNIT_NAME"
    sleep 1
    systemctl --no-pager status "$UNIT_NAME" | head -15 || true
  fi
elif [[ "$RESTART" -eq 1 ]]; then
  systemctl start "$UNIT_NAME"
fi

WEB_HOST="${PUBLIC_ADDR:-}"
if [[ -z "$WEB_HOST" && -f "$ENV_FILE" ]]; then
  WEB_HOST="$(grep -E '^MASTER_PUBLIC_ADDR=' "$ENV_FILE" | head -1 | cut -d= -f2-)"
fi
WEB_HOST="${WEB_HOST%%,*}"
DB_URL="$(grep -E '^MASTER_DATABASE_URL=' "$ENV_FILE" 2>/dev/null | cut -d= -f2-)"

echo
printf '\033[1;32m══════════════════════════════════════════════════\033[0m\n'
printf '\033[1;32m  relay-master %s 安装完成\033[0m\n' "$VERSION"
printf '\033[1;32m══════════════════════════════════════════════════\033[0m\n'
echo
if [[ -n "$WEB_DOMAIN" ]]; then
  log "Web 控制台:      https://$WEB_DOMAIN"
else
  log "Web 控制台:      http://${WEB_HOST:-<this-host>}:7080"
fi
log "gRPC 端口:       ${WEB_HOST:-<this-host>}:7443"
echo
[[ -n "$DB_URL" ]] && log "数据库连接:      $DB_URL"
log "配置文件:        $ENV_FILE"
echo
log "日志查看:        journalctl -u $UNIT_NAME -f"
log "服务状态:        systemctl status $UNIT_NAME"
log "重启服务:        systemctl restart $UNIT_NAME"

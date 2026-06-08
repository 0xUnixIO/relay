#!/usr/bin/env bash
# Install or upgrade relay-node from a GitHub release.
#
# Don't run this by hand. The relay master Web UI generates a copy-paste
# command for you when you create a node ("Nodes → New Node"); the URL
# embeds your master endpoint, node id, and one-shot enrollment token.
#
# Manual usage (if you know what you are doing — script self-elevates,
# no sudo needed):
#   bash <(curl -fsSL https://raw.githubusercontent.com/0xUnixIO/relay/main/install-node.sh) \
#     --master https://master.example.com:7443 \
#     --node-id node-01 \
#     --token <ENROLLMENT_TOKEN> \
#     --ca-cert <BASE64_CA_CERT>
#
# Flags:
#   --master <url>         gRPC endpoint of the master (required for first install)
#   --node-id <id>         node identifier as registered on the master (required)
#   --token <token>        enrollment token shown in the master Web UI (required)
#   --ca-cert <b64>        base64 of the master CA cert PEM (required first install)
#   --enroll <url>         master enrollment endpoint (default: http://<master-host>:7080/api/v1/enroll)
#                          alias: --master-enroll-endpoint
#   --version <tag>        pin a specific release tag (default: latest)
#   --repo <owner/name>    override the GitHub repo (default: 0xUnixIO/relay)
#   --setup <url>          一键安装链接（从 master Web UI 复制），包含所有安装参数
#   --mirror <url>         GitHub 镜像前缀，用于国内加速（如 https://ghproxy.com/）
#   --update               upgrade-only: keep existing env / pki, no enrollment args needed
#   --non-interactive      never prompt (for automated callers like the updater)
#   --no-start             install but don't enable/start the service
#   --uninstall            stop service, remove binary + unit. Then asks
#                          interactively whether to wipe env + node PKI
#                          (default: keep).

set -euo pipefail

REPO="0xUnixIO/relay"
VERSION="latest"
INCLUDE_PRERELEASE=0
MASTER=""
NODE_ID=""
NODE_TOKEN=""
NODE_CA_CERT_B64=""
ENROLL_ENDPOINT=""
SETUP_URL=""
MIRROR=""
START=1
UNINSTALL=0
UPDATE_ONLY=0
NON_INTERACTIVE=0

log()  { printf '\033[1;34m==>\033[0m %s\n' "$*"; }
warn() { printf '\033[1;33m!!\033[0m %s\n' "$*" >&2; }
die()  { printf '\033[1;31mxx\033[0m %s\n' "$*" >&2; exit 1; }

while [[ $# -gt 0 ]]; do
  case "$1" in
    --master)          MASTER="$2"; shift 2 ;;
    --node-id)         NODE_ID="$2"; shift 2 ;;
    --token)           NODE_TOKEN="$2"; shift 2 ;;
    --ca-cert)         NODE_CA_CERT_B64="$2"; shift 2 ;;
    --setup)            SETUP_URL="$2"; shift 2 ;;
    --enroll|--master-enroll-endpoint)
                       ENROLL_ENDPOINT="$2"; shift 2 ;;
    --version)         VERSION="$2"; shift 2 ;;
    --prerelease)      INCLUDE_PRERELEASE=1; shift ;;
    --repo)            REPO="$2"; shift 2 ;;
    --mirror)          MIRROR="${2%/}/"; shift 2 ;;
    --no-start)        START=0; shift ;;
    --uninstall)       UNINSTALL=1; shift ;;
    --update)          UPDATE_ONLY=1; shift ;;
    --non-interactive) NON_INTERACTIVE=1; shift ;;
    -h|--help)         sed -n '2,32p' "$0"; exit 0 ;;
    *) die "unknown flag: $1" ;;
  esac
done

BIN_NAME="relay-node"
UNIT_NAME="relay-node"
ETC_DIR="/etc/relay-node"
ENV_FILE="$ETC_DIR/relay-node.env"
DATA_DIR="/var/lib/relay-node"
BIN_LINK="/usr/local/bin/$BIN_NAME"

[[ $EUID -eq 0 ]] || {
  # Re-exec under sudo. Persist the script to a tempfile first so sudo
  # can read it even when invoked via `bash <(curl …)` (where $0 is
  # /dev/fd/N which sudo's default closefrom would drop).
  command -v sudo >/dev/null 2>&1 || die "must run as root (no sudo found; re-run as root)"
  tmp="$(mktemp /tmp/relay-install-node.XXXXXX.sh)"
  case "$0" in
    /dev/fd/*) cat "$0" > "$tmp" ;;
    bash|-bash|sh|-sh) die "must run as root (try: sudo bash -c \"\$(curl -fsSL …)\")" ;;
    *) cp "$0" "$tmp" ;;
  esac
  chmod +x "$tmp"
  exec sudo bash "$tmp" "$@"
}
# Self-elevated runs leave /tmp/relay-install-node.XXXXXX.sh behind. Clean
# it up on exit if we were the elevated process.
case "$0" in /tmp/relay-install-node.*.sh) trap 'rm -f "$0"' EXIT ;; esac

# ── 发行版 / libc / 包管理器 / init 系统探测 ───────────────────────────────────
OS_ID=""
[[ -r /etc/os-release ]] && OS_ID="$(. /etc/os-release 2>/dev/null; echo "${ID:-}")"

# libc：Alpine 等使用 musl，需要对应的 musl 静态二进制（glibc 产物跑不起来）。
LIBC="gnu"
if [[ "$OS_ID" == "alpine" ]] || ldd --version 2>&1 | grep -qi musl \
   || ls /lib/ld-musl-* >/dev/null 2>&1; then
  LIBC="musl"
fi

# 包管理器
PKG=""
if   command -v apt-get >/dev/null 2>&1; then PKG="apt"
elif command -v apk     >/dev/null 2>&1; then PKG="apk"
fi

# init 系统：systemd 优先，其次 OpenRC（Alpine）。
INIT=""
if command -v systemctl >/dev/null 2>&1 && [[ -d /run/systemd/system ]]; then
  INIT="systemd"
elif command -v rc-service >/dev/null 2>&1 || command -v openrc >/dev/null 2>&1; then
  INIT="openrc"
elif command -v systemctl >/dev/null 2>&1; then
  INIT="systemd"
fi

# ── 服务管理抽象（systemd / OpenRC）────────────────────────────────────────────
svc_unit_path() {
  case "$INIT" in
    systemd) echo "/etc/systemd/system/${UNIT_NAME}.service" ;;
    openrc)  echo "/etc/init.d/${UNIT_NAME}" ;;
  esac
}
svc_is_active() {
  case "$INIT" in
    systemd) systemctl is-active --quiet "$UNIT_NAME" 2>/dev/null ;;
    openrc)  rc-service "$UNIT_NAME" status >/dev/null 2>&1 ;;
    *)       return 1 ;;
  esac
}
svc_stop() {
  case "$INIT" in
    systemd) systemctl stop "$UNIT_NAME" ;;
    openrc)  rc-service "$UNIT_NAME" stop 2>/dev/null || true ;;
  esac
}
svc_start() {
  case "$INIT" in
    systemd) systemctl start "$UNIT_NAME" ;;
    openrc)  rc-service "$UNIT_NAME" start ;;
  esac
}
svc_enable_start() {
  case "$INIT" in
    systemd) systemctl enable --now "$UNIT_NAME" ;;
    openrc)  rc-update add "$UNIT_NAME" default >/dev/null 2>&1 || true
             rc-service "$UNIT_NAME" restart ;;
  esac
}
svc_disable() {
  case "$INIT" in
    systemd) systemctl disable --now "$UNIT_NAME" 2>/dev/null || true ;;
    openrc)  rc-service "$UNIT_NAME" stop 2>/dev/null || true
             rc-update del "$UNIT_NAME" default 2>/dev/null || true ;;
  esac
}
svc_status() {
  case "$INIT" in
    systemd) systemctl --no-pager status "$UNIT_NAME" | head -15 || true ;;
    openrc)  rc-service "$UNIT_NAME" status || true ;;
  esac
}
svc_reload_daemon() {
  case "$INIT" in
    systemd) systemctl daemon-reload ;;
    openrc)  : ;;
  esac
}
svc_install_unit() {
  case "$INIT" in
    systemd)
      curl -fsSL "${GH_RAW}/$REPO/$DEPLOY_REF/deploy/systemd/${UNIT_NAME}.service" \
           -o "/etc/systemd/system/${UNIT_NAME}.service"
      ;;
    openrc)
      curl -fsSL "${GH_RAW}/$REPO/$DEPLOY_REF/deploy/openrc/${UNIT_NAME}" \
           -o "/etc/init.d/${UNIT_NAME}"
      chmod 0755 "/etc/init.d/${UNIT_NAME}"
      ;;
  esac
}

if [[ "$UNINSTALL" -eq 1 ]]; then
  log "stopping $UNIT_NAME"
  svc_disable
  rm -f "$(svc_unit_path)"
  rm -f "$BIN_LINK"
  svc_reload_daemon
  log "removed binary + service unit"

  PURGE=0
  if { [[ -t 0 ]] || [[ -e /dev/tty ]]; }; then
    [[ -t 0 ]] || exec </dev/tty
    echo
    log "保留在磁盘上的内容："
    [[ -f "$ENV_FILE" ]] && log "  - $ENV_FILE"
    [[ -d /var/lib/relay-node/pki ]] && log "  - /var/lib/relay-node/pki/   # 节点证书 + 私钥"
    echo
    read -r -p "是否一并清除以上内容（不可恢复）？[y/N] " ans
    case "${ans:-N}" in [Yy]*) PURGE=1 ;; esac
  fi

  if [[ "$PURGE" -eq 1 ]]; then
    log "wiping $ETC_DIR and /var/lib/relay-node"
    rm -rf "$ETC_DIR" /var/lib/relay-node
    if id relay >/dev/null 2>&1; then
      log "removing system user 'relay'"
      if command -v userdel >/dev/null 2>&1; then
        userdel relay 2>/dev/null || true
      else
        deluser relay 2>/dev/null || true   # BusyBox (Alpine)
      fi
    fi
    log "purged. Nothing left on disk."
  fi
  exit 0
fi

OS="$(uname -s)"
ARCH="$(uname -m)"
case "$OS-$ARCH" in
  Linux-x86_64)              ARCH_T="x86_64" ;;
  Linux-aarch64|Linux-arm64) ARCH_T="aarch64" ;;
  *) die "unsupported platform: $OS $ARCH" ;;
esac
# libc 决定下载 glibc 还是 musl 产物（musl 供 Alpine 等）。
TARGET="${ARCH_T}-unknown-linux-${LIBC}"

GH="${MIRROR}https://github.com"
GH_API="https://api.github.com"
GH_RAW="${MIRROR}https://raw.githubusercontent.com"

ensure_base_pkgs() {
  # 抽象需求（与具体包名解耦），再按包管理器映射安装。
  local need=()
  command -v curl       >/dev/null || need+=(curl)
  command -v tar        >/dev/null || need+=(tar)
  command -v sha256sum  >/dev/null || need+=(sha256sum)
  [[ -e /etc/ssl/certs/ca-certificates.crt ]] || need+=(ca-certificates)
  # OpenRC 下以非 root 绑定 <1024 端口依赖 setcap（libcap）。
  [[ "$INIT" == "openrc" ]] && { command -v setcap >/dev/null || need+=(setcap); }
  [[ ${#need[@]} -eq 0 ]] && return 0
  [[ "$EUID" -eq 0 ]] || return 0

  local pkgs=()
  case "$PKG" in
    apt)
      for n in "${need[@]}"; do case "$n" in
        sha256sum) pkgs+=(coreutils) ;;
        setcap)    pkgs+=(libcap2-bin) ;;
        *)         pkgs+=("$n") ;;
      esac; done
      log "安装基础依赖: ${pkgs[*]}"
      apt-get update -qq
      DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends "${pkgs[@]}" >/dev/null
      ;;
    apk)
      for n in "${need[@]}"; do case "$n" in
        sha256sum) pkgs+=(coreutils) ;;   # BusyBox 通常已自带，缺失时兜底
        setcap)    pkgs+=(libcap) ;;
        *)         pkgs+=("$n") ;;
      esac; done
      log "安装基础依赖: ${pkgs[*]}"
      apk add --no-cache "${pkgs[@]}" >/dev/null
      ;;
    *) return 0 ;;
  esac
}
ensure_base_pkgs

command -v curl       >/dev/null || die "curl is required"
command -v tar        >/dev/null || die "tar is required"
command -v sha256sum  >/dev/null || die "sha256sum is required (coreutils)"
[[ -n "$INIT" ]] || die "no supported init system found (need systemd or OpenRC)"

# 从扁平 JSON 中取字符串字段。优先 python3（健壮），缺失时（如 Alpine 默认
# 无 python3）回退到 grep/sed —— setup JSON 各字段值均为无引号转义的简单串。
json_str() { # $1=json  $2=key
  if command -v python3 >/dev/null 2>&1; then
    printf '%s' "$1" | python3 -c "import sys,json; print(json.load(sys.stdin).get('$2',''))"
  else
    printf '%s' "$1" \
      | grep -oE "\"$2\"[[:space:]]*:[[:space:]]*\"[^\"]*\"" \
      | head -1 | sed -E "s/.*:[[:space:]]*\"([^\"]*)\"/\1/"
  fi
}

if [[ -n "$SETUP_URL" ]]; then
  log "fetching setup parameters"
  SETUP_JSON="$(curl -fsSL "$SETUP_URL")" || die "无法获取安装链接，请检查链接是否有效"
  MASTER="$(          json_str "$SETUP_JSON" master)"
  ENROLL_ENDPOINT="$( json_str "$SETUP_JSON" enroll)"
  NODE_ID="$(         json_str "$SETUP_JSON" node_id)"
  NODE_TOKEN="$(      json_str "$SETUP_JSON" token)"
  NODE_CA_CERT_B64="$(json_str "$SETUP_JSON" ca_cert)"
fi

if [[ "$VERSION" == "latest" ]]; then
  if [[ "$INCLUDE_PRERELEASE" -eq 1 ]]; then
    command -v python3 >/dev/null 2>&1 \
      || die "--prerelease 需要 python3（或改用 --version <tag> 指定具体版本）"
    VERSION="$(curl -fsSL "${GH_API}/repos/$REPO/releases?per_page=20" \
      | python3 -c "import sys,json; rs=[r for r in json.load(sys.stdin) if r['prerelease']]; print(rs[0]['tag_name'] if rs else '')")"
    [[ -n "$VERSION" ]] || die "failed to resolve latest pre-release"
    log "latest pre-release: $VERSION"
  else
    VERSION="$(curl -fsSL "${GH_API}/repos/$REPO/releases/latest" \
      | grep -oE '"tag_name": *"[^"]+"' | head -1 | cut -d'"' -f4)"
    [[ -n "$VERSION" ]] || die "failed to resolve latest version (rate-limited?)"
  fi
fi

ARCHIVE="relay-node-${VERSION}-${TARGET}.tar.gz"
BASE="${GH}/$REPO/releases/download/$VERSION"

log "installing relay-node $VERSION for $TARGET"

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

log "downloading $ARCHIVE"
curl -fsSL "$BASE/$ARCHIVE"   -o "$TMP/$ARCHIVE"
curl -fsSL "$BASE/SHA256SUMS" -o "$TMP/SHA256SUMS"

log "verifying sha256"
( cd "$TMP" && grep " $ARCHIVE\$" SHA256SUMS | sha256sum -c - ) \
  || die "checksum mismatch"

tar -xzf "$TMP/$ARCHIVE" -C "$TMP"
DIR="$TMP/relay-node-${VERSION}-${TARGET}"
[[ -f "$DIR/$BIN_NAME" ]] || die "$BIN_NAME not found in archive"

RESTART=0
if svc_is_active; then
  log "stopping running $UNIT_NAME before upgrade"
  svc_stop
  RESTART=1
fi

if ! id relay >/dev/null 2>&1; then
  log "creating system user 'relay'"
  if command -v useradd >/dev/null 2>&1; then
    useradd --system --no-create-home --shell /usr/sbin/nologin relay
  else
    # BusyBox (Alpine)：adduser 语义不同，需先建组。
    addgroup -S relay 2>/dev/null || true
    adduser -S -D -H -s /sbin/nologin -G relay relay
  fi
fi

log "installing $DATA_DIR/$BIN_NAME"
mkdir -p "$DATA_DIR"
chown relay:relay "$DATA_DIR"
chmod 0700 "$DATA_DIR"
install -m 0755 "$DIR/$BIN_NAME" "$DATA_DIR/$BIN_NAME.new"
chown relay:relay "$DATA_DIR/$BIN_NAME.new"
mv -f "$DATA_DIR/$BIN_NAME.new" "$DATA_DIR/$BIN_NAME"
ln -sfn "$DATA_DIR/$BIN_NAME" "${BIN_LINK}.new"
mv -Tf "${BIN_LINK}.new" "$BIN_LINK"

# OpenRC 无 systemd 的 AmbientCapabilities，用文件 capability 让非 root
# 的 relay 用户也能绑定 <1024 端口。setcap 须在最终 mv 之后执行。
if [[ "$INIT" == "openrc" ]]; then
  if command -v setcap >/dev/null 2>&1; then
    setcap 'cap_net_bind_service=+ep' "$DATA_DIR/$BIN_NAME" \
      || warn "setcap 失败：绑定 <1024 端口可能需要 root"
  else
    warn "未找到 setcap（libcap），绑定 <1024 端口可能失败"
  fi
fi

mkdir -p "$ETC_DIR"

PKI_DIR="/var/lib/relay-node/pki"

if [[ -n "$NODE_TOKEN" ]]; then
  # 有 token → 首装或重新注册，直接覆盖
  if [[ -z "$MASTER" || -z "$NODE_ID" || -z "$NODE_CA_CERT_B64" ]]; then
    die "enrollment requires --master, --node-id, --token and --ca-cert (get them from the master Web UI)"
  fi
  if [[ -d "$PKI_DIR" ]]; then
    log "wiping $PKI_DIR for re-enrollment"
    rm -rf "$PKI_DIR"
  fi
  if [[ -z "$ENROLL_ENDPOINT" ]]; then
    HOST_PORT="${MASTER#http://}"; HOST_PORT="${HOST_PORT#https://}"
    HOST="${HOST_PORT%%/*}"; HOST="${HOST%%:*}"
    ENROLL_ENDPOINT="http://${HOST}:7080/api/v1/enroll"
  fi
  log "writing $ENV_FILE"
  cat >"$ENV_FILE" <<EOF
# relay-node configuration (managed by install-node.sh)
NODE_MASTER_ENDPOINT=$MASTER
NODE_MASTER_ENROLL_ENDPOINT=$ENROLL_ENDPOINT
NODE_ID=$NODE_ID
NODE_TOKEN=$NODE_TOKEN
NODE_CA_CERT_B64=$NODE_CA_CERT_B64
EOF
  chmod 0640 "$ENV_FILE"
  chown root:relay "$ENV_FILE"
elif [[ "$UPDATE_ONLY" -eq 1 ]]; then
  if [[ ! -f "$ENV_FILE" ]] || [[ ! -f "$PKI_DIR/node.crt" ]]; then
    die "--update requires an existing relay-node install ($ENV_FILE + $PKI_DIR)"
  fi
  log "--update mode: keeping existing env + pki"
elif [[ ! -f "$ENV_FILE" ]] || [[ ! -f "$PKI_DIR/node.crt" ]]; then
  die "first install requires --master, --node-id, --token and --ca-cert (get them from the master Web UI)"
else
  log "$ENV_FILE and pki already populated — upgrading binary only"
fi

# Pick a ref to fetch deploy/ files from. For pinned versions (e.g. v0.2.0)
# fetch from the corresponding tag for reproducibility; only fall back to
# main for `--version latest` callers (which means we already resolved a
# specific tag above, so that branch never actually executes — but be
# defensive).
DEPLOY_REF="$VERSION"
case "$DEPLOY_REF" in v*) ;; *) DEPLOY_REF="main" ;; esac

log "installing $INIT service unit"
svc_install_unit
svc_reload_daemon

if [[ "$START" -eq 1 ]]; then
  log "enabling + starting $UNIT_NAME"
  svc_enable_start
  sleep 1
  svc_status
elif [[ "$RESTART" -eq 1 ]]; then
  svc_start
fi

if [[ "$INIT" == "systemd" ]]; then
  log "完成。日志查看：journalctl -u $UNIT_NAME -f"
else
  log "完成。日志查看：rc-service $UNIT_NAME status  （详细日志见 /var/log/messages）"
fi

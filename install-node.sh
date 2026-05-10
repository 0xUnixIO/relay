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
LIB_DIR="/usr/local/lib/relay-node"
BIN_LINK="/usr/local/bin/$BIN_NAME"

[[ $EUID -eq 0 ]] || {
  # Re-exec under sudo. Persist the script to a tempfile first so sudo
  # can read it even when invoked via `bash <(curl …)` (where $0 is
  # /dev/fd/N which sudo's default closefrom would drop).
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

if [[ "$UNINSTALL" -eq 1 ]]; then
  log "stopping $UNIT_NAME"
  systemctl disable --now "$UNIT_NAME" 2>/dev/null || true
  systemctl disable --now "${UNIT_NAME}-updater.path" 2>/dev/null || true
  systemctl disable --now "${UNIT_NAME}-updater.service" 2>/dev/null || true
  rm -f "/etc/systemd/system/${UNIT_NAME}.service"
  rm -f "/etc/systemd/system/${UNIT_NAME}-updater.service"
  rm -f "/etc/systemd/system/${UNIT_NAME}-updater.path"
  rm -f "$BIN_LINK"
  rm -rf "$LIB_DIR"
  systemctl daemon-reload
  log "removed binary + systemd units"

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
      userdel relay 2>/dev/null || true
    fi
    log "purged. Nothing left on disk."
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

GH="${MIRROR}https://github.com"
GH_API="https://api.github.com"
GH_RAW="${MIRROR}https://raw.githubusercontent.com"

ensure_base_pkgs() {
  local need=()
  command -v curl       >/dev/null || need+=(curl)
  command -v tar        >/dev/null || need+=(tar)
  command -v sha256sum  >/dev/null || need+=(coreutils)
  [[ -e /etc/ssl/certs/ca-certificates.crt ]] || need+=(ca-certificates)
  [[ ${#need[@]} -eq 0 ]] && return 0
  if [[ "$EUID" -ne 0 ]] || ! command -v apt-get >/dev/null; then
    return 0
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

if [[ "$VERSION" == "latest" ]]; then
  if [[ "$INCLUDE_PRERELEASE" -eq 1 ]]; then
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

ARCHIVE="relay-${VERSION}-${TARGET}.tar.gz"
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
DIR="$TMP/relay-${VERSION}-${TARGET}"
[[ -f "$DIR/$BIN_NAME" ]] || die "$BIN_NAME not found in archive"

RESTART=0
if systemctl is-active --quiet "$UNIT_NAME" 2>/dev/null; then
  log "stopping running $UNIT_NAME before upgrade"
  systemctl stop "$UNIT_NAME"
  RESTART=1
fi

log "installing $LIB_DIR/$BIN_NAME"
mkdir -p "$LIB_DIR"
install -m 0755 "$DIR/$BIN_NAME" "$LIB_DIR/$BIN_NAME.new"
mv -f "$LIB_DIR/$BIN_NAME.new" "$LIB_DIR/$BIN_NAME"

# 原子替换 PATH 下的软链接
ln -sfn "$LIB_DIR/$BIN_NAME" "${BIN_LINK}.new"
mv -Tf "${BIN_LINK}.new" "$BIN_LINK"

if ! id relay >/dev/null 2>&1; then
  log "creating system user 'relay'"
  useradd --system --no-create-home --shell /usr/sbin/nologin relay
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

log "installing systemd unit"
curl -fsSL "${GH_RAW}/$REPO/$DEPLOY_REF/deploy/systemd/${UNIT_NAME}.service" \
     -o "/etc/systemd/system/${UNIT_NAME}.service"

log "installing relay-node-updater (root-level upgrade helper)"
mkdir -p "$LIB_DIR"
curl -fsSL "${GH_RAW}/$REPO/$DEPLOY_REF/deploy/systemd/${UNIT_NAME}-updater" \
     -o "$LIB_DIR/${UNIT_NAME}-updater"
chmod 0755 "$LIB_DIR/${UNIT_NAME}-updater"
curl -fsSL "${GH_RAW}/$REPO/$DEPLOY_REF/deploy/systemd/${UNIT_NAME}-updater.service" \
     -o "/etc/systemd/system/${UNIT_NAME}-updater.service"
curl -fsSL "${GH_RAW}/$REPO/$DEPLOY_REF/deploy/systemd/${UNIT_NAME}-updater.path" \
     -o "/etc/systemd/system/${UNIT_NAME}-updater.path"

systemctl daemon-reload

log "enabling relay-node-updater path watcher"
systemctl enable --now "${UNIT_NAME}-updater.path" 2>/dev/null || true

if [[ "$START" -eq 1 ]]; then
  log "enabling + starting $UNIT_NAME"
  systemctl enable --now "$UNIT_NAME"
  sleep 1
  systemctl --no-pager status "$UNIT_NAME" | head -15 || true
elif [[ "$RESTART" -eq 1 ]]; then
  systemctl start "$UNIT_NAME"
fi

log "完成。日志查看：journalctl -u $UNIT_NAME -f"

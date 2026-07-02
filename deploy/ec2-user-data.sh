#!/usr/bin/env bash
# EC2 user-data bootstrap for the inicio faucet API (Amazon Linux 2023).
#
# Builds the faucet image on the instance, GENERATES the 4 faucet signing keys (.mac) locally on
# the box (no Secrets Manager — testnet keys are low-value), writes faucet.toml + a host for Caddy,
# and runs faucet + Caddy via docker compose. Caddy gets auto-HTTPS for <public-ip>.nip.io.
# State (sqlite) + keys live in $APP_DIR/faucets on the instance's EBS. Keys are generated on first
# boot and kept if present; replacing the instance yields NEW faucet accounts (new IDs) — fine for testnet.
#
# Prereqs: a security group allowing 80 + 443; SSM for admin (no SSH port). The frontend is hosted
# on Amplify separately; aws-provision.sh injects CORS_ALLOWED_ORIGINS so the cross-origin UI is allowed.
set -euo pipefail

REPO_URL="https://github.com/inicio-labs/inicio-faucet.git"
APP_DIR="/opt/inicio-faucet"
ENDPOINT="https://rpc.testnet.miden.io"
PROVER_URL="https://tx-prover.testnet.miden.io"
MAX_SUPPLY="100000000000000000"   # faucet max supply: 1e9 whole tokens at 8 decimals
# Per-request mint cap (base units): 1,000,000 whole tokens at 8 decimals.
MAX_MINT="100000000000000"
# Amplify frontend origin(s) for CORS (aws-provision.sh injects this). Empty = none allowed.
CORS_ALLOWED_ORIGINS="${CORS_ALLOWED_ORIGINS:-}"
# "SYMBOL:Name:decimals" per token.
TOKENS=("IMIDEN:Inicio Miden:8" "IETH:Inicio ETH:8" "IBTC:Inicio BTC:8" "IUSDT:Inicio USDT:8")

# --- Docker + compose plugin + buildx + git ---
dnf -y install docker git
systemctl enable --now docker
mkdir -p /usr/local/lib/docker/cli-plugins
curl -fsSL https://github.com/docker/compose/releases/latest/download/docker-compose-linux-x86_64 \
  -o /usr/local/lib/docker/cli-plugins/docker-compose
chmod +x /usr/local/lib/docker/cli-plugins/docker-compose
# buildx: AL2023 ships an older one, but `docker compose build` needs >= 0.17. Pin a known
# version — an API lookup piped to `grep -m1` trips pipefail via SIGPIPE under `set -e`.
BX_VER="v0.35.0"
curl -fsSL "https://github.com/docker/buildx/releases/download/${BX_VER}/buildx-${BX_VER}.linux-amd64" \
  -o /usr/local/lib/docker/cli-plugins/docker-buildx
chmod +x /usr/local/lib/docker/cli-plugins/docker-buildx

# --- swap so the heavy one-time Rust build fits on a small instance ---
if ! swapon --show | grep -q /swapfile; then
  fallocate -l 8G /swapfile 2>/dev/null || dd if=/dev/zero of=/swapfile bs=1M count=8192
  chmod 600 /swapfile
  mkswap /swapfile
  swapon /swapfile
  echo "/swapfile none swap sw 0 0" >> /etc/fstab
fi

# --- public hostname for Caddy's cert ---
# aws-provision.sh injects `export FAUCET_HOST=<eip>.nip.io` right after the shebang so the cert
# matches the (stable) Elastic IP regardless of boot-time IP. Fallback: derive from IMDSv2.
if [ -z "${FAUCET_HOST:-}" ]; then
  IMDS_TOKEN=$(curl -fsS -X PUT http://169.254.169.254/latest/api/token \
    -H "X-aws-ec2-metadata-token-ttl-seconds: 300")
  PUBLIC_IP=$(curl -fsS -H "X-aws-ec2-metadata-token: $IMDS_TOKEN" \
    http://169.254.169.254/latest/meta-data/public-ipv4)
  FAUCET_HOST="${PUBLIC_IP}.nip.io"
fi
export FAUCET_HOST

# --- source ---
rm -rf "$APP_DIR"
git clone --depth 1 "$REPO_URL" "$APP_DIR"
cd "$APP_DIR"
mkdir -p faucets
chown 10001:10001 faucets   # so the uid-10001 container can write keys/stores here
# .env so every `docker compose` invocation gets FAUCET_HOST (build / run / up).
echo "FAUCET_HOST=$FAUCET_HOST" > .env

# --- faucet.toml ---
cors_toml="[]"
if [ -n "$CORS_ALLOWED_ORIGINS" ]; then cors_toml="[\"$CORS_ALLOWED_ORIGINS\"]"; fi
{
  cat <<TOML
[rpc]
endpoint = "$ENDPOINT"
timeout_ms = 30000
remote_prover_url = "$PROVER_URL"

[server]
bind = "0.0.0.0:8080"
max_batch_size = 256
static_dir = "static"
cors_allowed_origins = $cors_toml
TOML
  for t in "${TOKENS[@]}"; do
    sym=${t%%:*}; rest=${t#*:}; name=${rest%%:*}; dec=${rest##*:}
    lc=$(echo "$sym" | tr 'A-Z' 'a-z')
    cat <<TOML

[[tokens]]
symbol = "$sym"
name = "$name"
decimals = $dec
account_file = "faucets/${lc}.mac"
store_path = "faucets/${lc}.sqlite3"
keystore_path = "faucets/${lc}_keystore"
max_mint_amount = $MAX_MINT
TOML
  done
} > faucet.toml

# --- build the image, then generate any missing faucet .mac on the box (no Secrets Manager) ---
docker compose build
for t in "${TOKENS[@]}"; do
  sym=${t%%:*}; rest=${t#*:}; name=${rest%%:*}; dec=${rest##*:}
  lc=$(echo "$sym" | tr 'A-Z' 'a-z')
  if [ ! -f "faucets/${lc}.mac" ]; then
    docker compose run --rm --no-deps -T faucet \
      create-faucet --symbol "$sym" --name "$name" --decimals "$dec" \
      --max-supply "$MAX_SUPPLY" --out "faucets/${lc}.mac"
  fi
done
chown -R 10001:10001 faucets

# --- run (faucet + Caddy) ---
docker compose up -d

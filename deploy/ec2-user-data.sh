#!/usr/bin/env bash
# EC2 user-data bootstrap for the inicio faucet API (Amazon Linux 2023).
#
# Builds the faucet image on the instance, fetches the signing keys (.mac) from AWS
# Secrets Manager, writes faucet.toml + a host for Caddy, and runs faucet + Caddy via
# docker compose. Caddy gets auto-HTTPS for <public-ip>.nip.io. State (sqlite) lives in
# $APP_DIR/faucets; keys are re-fetched from Secrets Manager on every boot, so a
# replaced instance returns as the SAME faucet accounts.
#
# Prereqs (see README "Run on EC2"): the 4 .mac stored as binary secrets named
# inicio-faucet/<sym>.mac; the instance's IAM role granting secretsmanager:GetSecretValue;
# a security group allowing 80 + 443 (and 22 from your IP). The frontend is hosted on
# Amplify separately; set CORS_ALLOWED_ORIGINS to its URL once known (then re-run / restart).
set -euo pipefail

REGION="${AWS_REGION:-us-east-1}"
REPO_URL="https://github.com/inicio-labs/inicio-faucet.git"
APP_DIR="/opt/inicio-faucet"
ENDPOINT="https://rpc.devnet.miden.io"
PROVER_URL="https://tx-prover.devnet.miden.io"
# Per-request mint cap (base units). 1000 whole tokens at 8 decimals = mitigation for the
# unauthenticated API; tune as needed. Rate-limiting in Caddy is the recommended fast-follow.
MAX_MINT="100000000000"
# Set to the Amplify frontend URL once deployed, e.g. https://faucet.example.com
# (empty until then; the cross-origin UI won't be allowed until this is set + faucet restarted).
CORS_ALLOWED_ORIGINS="${CORS_ALLOWED_ORIGINS:-}"
# "SYMBOL:Name:decimals" per token.
TOKENS=("IMIDEN:Inicio Miden:8" "IETH:Inicio ETH:8" "IBTC:Inicio BTC:8" "IUSDT:Inicio USDT:8")

# --- Docker + compose plugin + git ---
dnf -y install docker git
systemctl enable --now docker
mkdir -p /usr/local/lib/docker/cli-plugins
curl -fsSL https://github.com/docker/compose/releases/latest/download/docker-compose-linux-x86_64 \
  -o /usr/local/lib/docker/cli-plugins/docker-compose
chmod +x /usr/local/lib/docker/cli-plugins/docker-compose

# --- public hostname for Caddy's cert (<public-ip>.nip.io via IMDSv2) ---
IMDS_TOKEN=$(curl -fsS -X PUT http://169.254.169.254/latest/api/token \
  -H "X-aws-ec2-metadata-token-ttl-seconds: 300")
PUBLIC_IP=$(curl -fsS -H "X-aws-ec2-metadata-token: $IMDS_TOKEN" \
  http://169.254.169.254/latest/meta-data/public-ipv4)
export FAUCET_HOST="${PUBLIC_IP}.nip.io"

# --- source ---
rm -rf "$APP_DIR"
git clone --depth 1 "$REPO_URL" "$APP_DIR"
cd "$APP_DIR"
mkdir -p faucets

# --- signing keys (.mac) from Secrets Manager + faucet.toml ---
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
    aws secretsmanager get-secret-value --region "$REGION" \
        --secret-id "inicio-faucet/${lc}.mac" --query SecretBinary --output text \
      | base64 -d > "faucets/${lc}.mac"
    chmod 600 "faucets/${lc}.mac"
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

# the container runs as uid 10001 (the image's user) and must own the data dir.
chown -R 10001:10001 faucets

# --- build + run (faucet + Caddy) ---
FAUCET_HOST="$FAUCET_HOST" docker compose up -d --build

#!/usr/bin/env bash
# EC2 user-data bootstrap for the inicio faucet (Amazon Linux 2023).
#
# Stands the faucet up on boot: installs Docker, pulls the signing keys (.mac) from
# AWS Secrets Manager, writes faucet.toml, and runs the container. State (sqlite +
# keys) lives under $DATA; the keys are re-fetched from Secrets Manager on every
# boot, so a replaced instance comes back as the SAME faucet accounts.
#
# Prerequisites (one-time, done locally — see README "Run on EC2"):
#   * the 4 faucet .mac files stored as binary secrets named  inicio-faucet/<sym>.mac
#   * the instance's IAM role granting  secretsmanager:GetSecretValue  on those
#   * a security group allowing 8080 only from your VPN / internal CIDRs
#   * the ghcr image published (public, or add a docker login below)
set -euo pipefail

IMAGE="${IMAGE:-ghcr.io/inicio-labs/inicio-faucet:latest}"   # pin a :vX.Y.Z in prod
DATA="${DATA:-/opt/faucet}"
REGION="${AWS_REGION:-us-east-1}"
APP_UID=10001                                                # the image's non-root user
TOKENS=("TOKA:Token A:8" "TOKB:Token B:8" "TOKC:Token C:8" "TOKD:Token D:8")

# --- Docker (Amazon Linux 2023; Ubuntu: apt-get install -y docker.io) ---
dnf -y install docker
systemctl enable --now docker

mkdir -p "$DATA/faucets"

# --- faucet.toml (config; not secret) ---
{
  cat <<TOML
[rpc]
endpoint = "https://rpc.devnet.miden.io"
timeout_ms = 10000
remote_prover_url = "https://tx-prover.devnet.miden.io"

[server]
bind = "0.0.0.0:8080"
max_batch_size = 256
static_dir = "static"
TOML
  for t in "${TOKENS[@]}"; do
    sym=${t%%:*}; rest=${t#*:}; name=${rest%%:*}; dec=${rest##*:}
    lc=$(echo "$sym" | tr 'A-Z' 'a-z')
    cat <<TOML

[[tokens]]
symbol = "$sym"
name = "$name"
decimals = $dec
account_file = "faucets/$lc.mac"
store_path = "faucets/$lc.sqlite3"
keystore_path = "faucets/${lc}_keystore"
TOML
  done
} > "$DATA/faucet.toml"

# --- signing keys (.mac) from Secrets Manager ---
for t in "${TOKENS[@]}"; do
  sym=${t%%:*}; lc=$(echo "$sym" | tr 'A-Z' 'a-z')
  aws secretsmanager get-secret-value --region "$REGION" \
      --secret-id "inicio-faucet/${lc}.mac" --query SecretBinary --output text \
    | base64 -d > "$DATA/faucets/${lc}.mac"
  chmod 600 "$DATA/faucets/${lc}.mac"
done

# The container runs as uid $APP_UID; it must own the data dir to read keys + write stores.
chown -R "$APP_UID:$APP_UID" "$DATA/faucets"

# --- run (if the ghcr package is private, docker login ghcr.io first) ---
docker pull "$IMAGE"
docker rm -f faucet 2>/dev/null || true
docker run -d --name faucet --restart unless-stopped \
  -p 8080:8080 \
  -v "$DATA/faucet.toml:/app/faucet.toml:ro" \
  -v "$DATA/faucets:/app/faucets" \
  "$IMAGE"

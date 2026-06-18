#!/usr/bin/env bash
# Provision the EC2 host for the inicio faucet API (run from your laptop).
#
# Creates (idempotently): an IAM role + instance profile (Secrets Manager read), a
# security group (80/443 public, 22 from your IP), an SSH key pair, an Elastic IP, and a
# t3.medium Amazon Linux 2023 instance running deploy/ec2-user-data.sh. The EIP is
# allocated first and its <eip>.nip.io host is baked into the user-data so Caddy's cert
# matches the stable IP. The instance builds the image once (swap covers the heavy compile)
# then runs it light.
#
# Prereqs: `aws` v2 configured (PROFILE), and the 4 secrets uploaded
# (inicio-faucet/<sym>.mac). Run from the repo root.
set -euo pipefail

PROFILE="${PROFILE:-inicio-faucet}"
REGION="${REGION:-us-east-1}"
NAME="inicio-faucet"
INSTANCE_TYPE="${INSTANCE_TYPE:-t3.medium}"
VOLUME_GB="${VOLUME_GB:-30}"
KEY_NAME="${KEY_NAME:-inicio-faucet-key}"
aws() { command aws --profile "$PROFILE" --region "$REGION" "$@"; }

echo "==> Caller identity"; aws sts get-caller-identity --output text

echo "==> AMI (Amazon Linux 2023)"
AMI=$(aws ssm get-parameter \
  --name /aws/service/ami-amazon-linux-latest/al2023-ami-kernel-default-x86_64 \
  --query Parameter.Value --output text)
echo "    $AMI"

echo "==> IAM role + instance profile"
if ! aws iam get-role --role-name "$NAME-ec2" >/dev/null 2>&1; then
  aws iam create-role --role-name "$NAME-ec2" \
    --assume-role-policy-document file://deploy/iam-trust-policy.json >/dev/null
fi
aws iam put-role-policy --role-name "$NAME-ec2" --policy-name secrets \
  --policy-document file://deploy/iam-secrets-policy.json
if ! aws iam get-instance-profile --instance-profile-name "$NAME-ec2" >/dev/null 2>&1; then
  aws iam create-instance-profile --instance-profile-name "$NAME-ec2" >/dev/null
  aws iam add-role-to-instance-profile --instance-profile-name "$NAME-ec2" \
    --role-name "$NAME-ec2"
fi

echo "==> Security group (in the default VPC)"
VPC=$(aws ec2 describe-vpcs --filters Name=isDefault,Values=true \
  --query 'Vpcs[0].VpcId' --output text)
SG=$(aws ec2 describe-security-groups --filters "Name=group-name,Values=$NAME-sg" \
  --query 'SecurityGroups[0].GroupId' --output text 2>/dev/null || echo "None")
if [ "$SG" = "None" ] || [ -z "$SG" ]; then
  SG=$(aws ec2 create-security-group --group-name "$NAME-sg" \
    --description "inicio faucet" --vpc-id "$VPC" --query GroupId --output text)
fi
MYIP=$(curl -fsS https://checkip.amazonaws.com | tr -d '[:space:]')
for rule in "80/0.0.0.0/0" "443/0.0.0.0/0" "22/${MYIP}/32"; do
  port=${rule%%/*}; cidr=${rule#*/}
  aws ec2 authorize-security-group-ingress --group-id "$SG" \
    --protocol tcp --port "$port" --cidr "$cidr" >/dev/null 2>&1 || true
done
echo "    $SG (80,443 public; 22 from $MYIP)"

echo "==> SSH key pair"
if ! aws ec2 describe-key-pairs --key-names "$KEY_NAME" >/dev/null 2>&1; then
  aws ec2 create-key-pair --key-name "$KEY_NAME" \
    --query KeyMaterial --output text > "$KEY_NAME.pem"
  chmod 600 "$KEY_NAME.pem"
  echo "    wrote $KEY_NAME.pem"
fi

echo "==> Elastic IP (allocate or reuse the one tagged $NAME)"
ALLOC=$(aws ec2 describe-addresses --filters "Name=tag:Name,Values=$NAME" \
  --query 'Addresses[0].AllocationId' --output text 2>/dev/null || echo "None")
if [ "$ALLOC" = "None" ] || [ -z "$ALLOC" ]; then
  ALLOC=$(aws ec2 allocate-address --domain vpc \
    --tag-specifications "ResourceType=elastic-ip,Tags=[{Key=Name,Value=$NAME}]" \
    --query AllocationId --output text)
fi
EIP=$(aws ec2 describe-addresses --allocation-ids "$ALLOC" \
  --query 'Addresses[0].PublicIp' --output text)
echo "    $EIP ($ALLOC)"

echo "==> Launch instance (FAUCET_HOST=${EIP}.nip.io baked into user-data)"
UD=$(mktemp)
awk -v h="export FAUCET_HOST=${EIP}.nip.io" 'NR==1{print; print h; next} {print}' \
  deploy/ec2-user-data.sh > "$UD"
IID=$(aws ec2 run-instances --image-id "$AMI" --instance-type "$INSTANCE_TYPE" \
  --key-name "$KEY_NAME" --security-group-ids "$SG" \
  --iam-instance-profile "Name=$NAME-ec2" \
  --user-data "file://$UD" \
  --block-device-mappings "[{\"DeviceName\":\"/dev/xvda\",\"Ebs\":{\"VolumeSize\":$VOLUME_GB,\"VolumeType\":\"gp3\"}}]" \
  --tag-specifications "ResourceType=instance,Tags=[{Key=Name,Value=$NAME}]" \
  --query 'Instances[0].InstanceId' --output text)
rm -f "$UD"
echo "    $IID — waiting for running"
aws ec2 wait instance-running --instance-ids "$IID"

echo "==> Associate Elastic IP"
aws ec2 associate-address --instance-id "$IID" --allocation-id "$ALLOC" >/dev/null

cat <<DONE

================================================================
 Instance:  $IID
 Elastic IP: $EIP
 API host:  https://${EIP}.nip.io   (ready ~25-40 min after boot — small box builds w/ swap)
================================================================
 Next:
  1. Set the Amplify app env  FAUCET_API_BASE = https://${EIP}.nip.io  and deploy the frontend.
  2. Once you have the Amplify URL, set the API's CORS to it:
       ssh -i $KEY_NAME.pem ec2-user@$EIP
       sudo sed -i 's#cors_allowed_origins = \[\]#cors_allowed_origins = ["<amplify-url>"]#' /opt/inicio-faucet/faucet.toml
       cd /opt/inicio-faucet && sudo docker compose restart faucet
  3. Verify:  curl https://${EIP}.nip.io/readyz
DONE

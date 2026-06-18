#!/usr/bin/env bash
# Provision the EC2 host for the inicio faucet API (run from your laptop).
#
# Creates (idempotently): an IAM role + instance profile (Secrets Manager read), a
# security group (80/443 public, 22 from your IP), an SSH key pair, a t3.large
# Amazon Linux 2023 instance running deploy/ec2-user-data.sh, and an Elastic IP.
#
# Prereqs: `aws` v2 configured (PROFILE), and the 4 secrets uploaded
# (inicio-faucet/<sym>.mac). Run from the repo root.
set -euo pipefail

PROFILE="${PROFILE:-inicio-faucet}"
REGION="${REGION:-us-east-1}"
NAME="inicio-faucet"
INSTANCE_TYPE="${INSTANCE_TYPE:-t3.large}"
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
aws iam create-role --role-name "$NAME-ec2" \
  --assume-role-policy-document file://deploy/iam-trust-policy.json >/dev/null 2>&1 || true
aws iam put-role-policy --role-name "$NAME-ec2" --policy-name secrets \
  --policy-document file://deploy/iam-secrets-policy.json
aws iam create-instance-profile --instance-profile-name "$NAME-ec2" >/dev/null 2>&1 || true
aws iam add-role-to-instance-profile --instance-profile-name "$NAME-ec2" \
  --role-name "$NAME-ec2" >/dev/null 2>&1 || true

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

echo "==> Launch instance"
sleep 10 # let the new instance profile propagate
IID=$(aws ec2 run-instances --image-id "$AMI" --instance-type "$INSTANCE_TYPE" \
  --key-name "$KEY_NAME" --security-group-ids "$SG" \
  --iam-instance-profile "Name=$NAME-ec2" \
  --user-data file://deploy/ec2-user-data.sh \
  --block-device-mappings "[{\"DeviceName\":\"/dev/xvda\",\"Ebs\":{\"VolumeSize\":$VOLUME_GB,\"VolumeType\":\"gp3\"}}]" \
  --tag-specifications "ResourceType=instance,Tags=[{Key=Name,Value=$NAME}]" \
  --query 'Instances[0].InstanceId' --output text)
echo "    $IID — waiting for running"
aws ec2 wait instance-running --instance-ids "$IID"

echo "==> Elastic IP"
ALLOC=$(aws ec2 allocate-address --domain vpc --query AllocationId --output text)
aws ec2 associate-address --instance-id "$IID" --allocation-id "$ALLOC" >/dev/null
EIP=$(aws ec2 describe-addresses --allocation-ids "$ALLOC" \
  --query 'Addresses[0].PublicIp' --output text)

cat <<DONE

================================================================
 Instance:  $IID
 Elastic IP: $EIP
 API host:  https://${EIP}.nip.io   (ready ~15 min after boot — the image builds on the box)
================================================================
 Next:
  1. Set the Amplify app env  FAUCET_API_BASE = https://${EIP}.nip.io  and deploy the frontend.
  2. Once you have the Amplify URL, set the API's CORS to it:
       ssh -i $KEY_NAME.pem ec2-user@$EIP
       sudo sed -i 's#cors_allowed_origins = \[\]#cors_allowed_origins = ["<amplify-url>"]#' /opt/inicio-faucet/faucet.toml
       cd /opt/inicio-faucet && sudo docker compose restart faucet
  3. Verify:  curl https://${EIP}.nip.io/readyz
DONE

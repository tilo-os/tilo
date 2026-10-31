#!/bin/bash
# Launch a fresh EC2 host for the trial VM (only when the stopped instance or
# its AMI is gone; restarting is in README.md). Prints the instance id and IP.
#
#   AWS_PROFILE=tilo ./launch-host.sh
set -euo pipefail
export AWS_REGION="${AWS_REGION:-us-west-2}"
# Nested virtualization exists only on 8th-generation Intel types (c8i, m8i, r8i).
type="${INSTANCE_TYPE:-m8i.2xlarge}"
ami=$(aws ec2 describe-images --owners 125523088429 \
  --filters 'Name=name,Values=Fedora-Cloud-Base-AmazonEC2.x86_64-44-*' 'Name=architecture,Values=x86_64' \
  --query 'reverse(sort_by(Images,&CreationDate))[0].ImageId' --output text)
vpc=$(aws ec2 describe-vpcs --filters Name=is-default,Values=true --query 'Vpcs[0].VpcId' --output text)
aws ec2 describe-key-pairs --key-names tilo-agent-trial >/dev/null 2>&1 \
  || aws ec2 import-key-pair --key-name tilo-agent-trial --public-key-material "fileb://$HOME/.ssh/id_ed25519.pub" >/dev/null
sg=$(aws ec2 describe-security-groups --filters Name=group-name,Values=tilo-agent-trial --query 'SecurityGroups[0].GroupId' --output text)
if [ "$sg" = None ]; then
  sg=$(aws ec2 create-security-group --group-name tilo-agent-trial --description "Tilo agent trial host: ssh only" --vpc-id "$vpc" --query GroupId --output text)
  aws ec2 authorize-security-group-ingress --group-id "$sg" --protocol tcp --port 22 --cidr 0.0.0.0/0 >/dev/null
fi
id=$(aws ec2 run-instances --image-id "${AMI:-$ami}" --instance-type "$type" --key-name tilo-agent-trial \
  --security-group-ids "$sg" --cpu-options NestedVirtualization=enabled \
  --block-device-mappings 'DeviceName=/dev/sda1,Ebs={VolumeSize=150,VolumeType=gp3,DeleteOnTermination=true}' \
  --tag-specifications 'ResourceType=instance,Tags=[{Key=Name,Value=tilo-agent-trial}]' \
  --query 'Instances[0].InstanceId' --output text)
aws ec2 wait instance-running --instance-ids "$id"
ip=$(aws ec2 describe-instances --instance-ids "$id" --query 'Reservations[0].Instances[0].PublicIpAddress' --output text)
echo "instance $id at $ip"

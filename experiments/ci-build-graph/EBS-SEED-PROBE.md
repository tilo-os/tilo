# AWS trusted Docker seed qualification

Prepared experiment; **not yet run or qualified**. `ebs_seed_probe.py` renders
resources without submitting them. It reuses the successfully exercised isolated
Docker seed script, but EBS restoration must be independently measured.

The infra checkout declares encrypted gp3 `agent-workspace-eks` storage and an
EBS CSI snapshot class of the same name. Terraform installs EBS CSI and the
snapshot controller. Broker credentials cannot list these cluster resources;
an AWS operator must verify their live state before applying the experiment.

1. Confirm a ready amd64 CI node, its actual availability zone, and conservative
   available capacity for 2 CPU/4 GiB plus Kata/QEMU. Do not rely only on the
   currently small RuntimeClass overhead. Preserve capacity for active work.
2. Render `--phase seed --node NODE --zone ZONE`. Review and create its explicit
   50 GiB raw-block PVC and bounded 900-second Job. The registry pull secret must
   exist in this cluster. No runner registration or workflow token is involved.
3. Require all three immutable image receipts, no containers or Docker volumes,
   and `CLEAN_DAEMON_STOP_AND_UNMOUNT`. Delete only this maintenance Job/Pod,
   retaining its explicit source PVC. Verify no remaining Pod references it.
4. Render and apply `--phase snapshot`; require readiness, immutable UID,
   reverse-bound snapshot content and source EBS volume ID. Record driver,
   source PV claim UID, size, volume mode, region and source zone. Preserve the
   source PVC and snapshot; their configured deletion policies are not retention
   protection against deliberate deletion.
5. Render `--phase clone` on the same node/zone for the first qualification.
   Require local inspection of all three digests before their pull, record
   pull timings, and require another clean stop/unmount. Delete only the
   disposable clone Job and PVC after recording results.

The helper must authorize the repository and verified runner identity before
attaching private bytes. EBS profiles are separate from existing strict ZFS
profiles: validate EBS driver, immutable snapshot/content/source bindings,
architecture, exact 50 GiB size and source AZ. Keep clone PVC provisioning under
WaitForFirstConsumer. Cross-AZ restoration and larger filesystem sizes need
separate qualification. A registry mirror alone would still transfer and
extract layers into every new VM.

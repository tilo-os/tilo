# Ali prep: a hosted Tilo desktop for an agent trial

A Tilo desktop on an EC2 host, for a prospective customer to point their own
AI agents (Claude Code, Codex, any MCP client) at and try two of their
computer-use tasks:

- **Document preparation:** fill PDF forms, correct text, combine and arrange
  pages, check the output, file the results.
- **Government-style applications:** a long multi-step form filled from source
  documents, saved and resumed, validation errors fixed, files uploaded into
  the right categories.

The tasks, source documents, mock portal and checker are the
[documents-and-forms pack](../documents-and-forms/README.md). This directory
holds what runs it: the host, the VM and how an agent connects.

## What exists

| Piece | Where |
| --- | --- |
| AWS | profile `tilo`, region `us-west-2` |
| Host | instance `i-0e9388afbcc3faafe`, m8i.2xlarge (8 vCPU, 32 GB) with nested virtualization, Fedora 44, 150 GB gp3, tag `Name=tilo-agent-trial` |
| Network | security group `tilo-agent-trial`: ssh (22) only, key-only; key pair `tilo-agent-trial` |
| Backup | AMI `ami-08dd32470bc181271` (`tilo-agent-trial-20261030`): the whole configured host |
| Host account | `tilo` (libvirt user session, linger on); `fedora` has sudo |
| VM | libvirt domain `tilo-trial`: 6 vCPU, 16 GB, `~/vm/trial.qcow2`, SPICE on host `127.0.0.1:5900`, guest ssh on host `127.0.0.1:2222` |
| Tilo image | `registry.tilo.so/tilo/tilo-vm:x86_64-ci`, built 2026-10-26 |
| Desktop | account `quickuser` / `tilo`, first-run identity setup done |
| Trial pack | `~/Documents/Trial/{Sources,Out,Submission}`; portal service `tilo-trial-portal` on `127.0.0.1:8471` inside the VM |
| Agent | `claude-code` admitted with Always allow; Browser, Files, PDF Viewer and Word Processor set to Allow for reading, changes and outside effects |

The instance is **stopped**. Stopped, it costs about $12 a month (the disk);
running, $0.42 an hour.

## Start it

```sh
aws login --profile tilo                  # when the session has expired
export AWS_PROFILE=tilo AWS_REGION=us-west-2
aws ec2 start-instances --instance-ids i-0e9388afbcc3faafe
aws ec2 wait instance-running --instance-ids i-0e9388afbcc3faafe
aws ec2 describe-instances --instance-ids i-0e9388afbcc3faafe \
  --query 'Reservations[0].Instances[0].PublicIpAddress' --output text
```

The public IP changes on every start. Put it in a copy of
[`ssh_config.example`](ssh_config.example) as `ssh_config`.

At boot the host's user unit `tilo-trial-vm.service` runs `~/vm/up.sh`: it
starts the VM and signs `quickuser` in at the greeter. About three minutes
after the instance is running, the desktop and its agent door are up:

```sh
ssh -F ssh_config trial-vm 'tilo-control actions list | head'
```

If the session was locked, unlock it:
`ssh -F ssh_config trial-vm 'tilo-control actions invoke session-lock unlock@session-lock --args-json "{\"password\": \"tilo\"}"'`.

## Stop it

Power the VM off first; it ignores libvirt's ACPI shutdown, so the unit's
stop (`~/vm/down.sh`) powers it off from inside:

```sh
ssh -F ssh_config trial-host 'systemctl --user stop tilo-trial-vm'
aws ec2 stop-instances --instance-ids i-0e9388afbcc3faafe
```

The host on the instance still has the older unit, whose stop is the ignored
ACPI shutdown; install the one here (`host/tilo-trial-vm.service` and
`host/down.sh` into `~/.config/systemd/user/` and `~/vm/`) on the next start.

## Watch the desktop

The VM's screen is SPICE on the host's loopback. Tunnel it and open any SPICE
client, such as virt-viewer's `remote-viewer`:

```sh
ssh -F ssh_config -N -L 5900:127.0.0.1:5900 trial-host
remote-viewer spice://127.0.0.1:5900
```

Keyboard and mouse go through the viewer. A still of the screen without a
viewer: `ssh -F ssh_config trial-host 'virsh -c qemu:///session screenshot tilo-trial /tmp/s.ppm'`
(the file is a PNG despite its name).

## Connect an agent

An agent acts through the session's agent door: resources and actions, no
pixels. With the participant's ssh public key added to
`~tilo/.ssh/authorized_keys` on the host and `~quickuser/.ssh/authorized_keys`
in the VM:

```sh
claude mcp add tilo -- ssh -T -J tilo@<HOST_IP> -p 2222 quickuser@127.0.0.1 tilo-runtime-mcp --connect
```

Tools: `tilo_ls`, `tilo_read`, `tilo_do`, `tilo_find`, `tilo_status` and the
`tilo_put_*` upload trio. A **new client name** (MCP `clientInfo.name`) waits
until someone admits it on the desktop. Answer from the host without a viewer:

```sh
ssh -F ssh_config trial-vm 'tilo-control actions list permission-prompts'
ssh -F ssh_config trial-vm 'tilo-control actions invoke permission-prompts allow-always@<prompt> --args-json "{\"confirm\": true, \"preset\": \"operator\"}"'
```

(the preset is accepted in lower case only). An admitted agent may open only
the apps the person allows it: Workspace settings, Agents, the agent, then
Allow per app and kind. That page lists only apps already open in the
workspace, so open them first:
`tilo-control actions invoke applications so.tilo.browser` (and
`so.tilo.pdf-viewer`, `so.tilo.file-explorer`, `so.tilo.word-processor`).

[`mcp_probe.py`](mcp_probe.py) checks the door from your machine:

```sh
TRIAL_SSH_CONFIG=ssh_config ./mcp_probe.py claude-code '[["tilo_ls", {"path": ""}]]'
```

## Run and check the tasks

The task cards the participant hands their agent are in
[`../documents-and-forms/TASKS.md`](../documents-and-forms/TASKS.md). Score an
attempt and start a fresh one inside the VM:

```sh
ssh -F ssh_config trial-vm 'python3 ~/.local/share/tilo-trial/documents-and-forms/check.py'
ssh -F ssh_config trial-vm '~/.local/share/tilo-trial/documents-and-forms/reset.sh'
```

To update the pack from a checkout:
`tar -C examples/agent-trials -cf - documents-and-forms | ssh -F ssh_config trial-vm 'mkdir -p /tmp/pack && tar -C /tmp/pack -xf - && bash /tmp/pack/documents-and-forms/install.sh'`.

## Not ready yet

These stop the trial from being handed over as is:

1. **The browser's page actions don't resolve over MCP** on this image.
   `tilo_find` lists `browser/browser.page#navigate`, `#read-page`,
   `#fill-page`, `#fill-page-files` and the rest, but `tilo_do` answers "no
   browser.page in browser": the page is a UI-bound resource
   (`tilo-ui-bound:v1:…`), which path lookup doesn't search. Every
   portal task depends on it. Check it against an image from current main.
2. **The new PDF actions aren't in the image.** `read-form-fields` and
   `insert-pages` landed on main on 2026-10-30, after the newest published
   image. Without them an agent has to guess form field names and build
   page entries by hand to merge PDFs.
3. **The idle lock** locks the session, and admin actions then refuse until
   it is unlocked. There is no setting or action found to turn it off.
4. **Per-app agent access has no action**; it is set by clicking in
   Workspace settings (done here through the dev session's test-control
   socket).

The next step was a dev image built from current main on the host
(`tilo device`/`tilo-vm image build --disk`, then `vm upgrade --image`), then
retesting 1 and fixing what remains.

## From scratch

Only when both the instance and the AMI are gone (`AMI=ami-08dd32470bc181271
./launch-host.sh` relaunches from the AMI instead, already configured):

1. `AWS_PROFILE=tilo ./launch-host.sh` launches the host and prints its IP.
2. As `fedora`: `sudo dnf -y install qemu-kvm libvirt-daemon-kvm libvirt-client edk2-ovmf passt qemu-img`,
   `sudo useradd -m -G kvm,libvirt tilo`, copy `~fedora/.ssh/authorized_keys`
   to `tilo`, `sudo loginctl enable-linger tilo`.
3. Copy `host/` to the host and, as `tilo`, run
   `FORGE_TOKEN=… host/create-vm.sh`. It downloads the published disk, moves
   the VM onto the dev channel's newest image and enables the unit.
4. Sign in (`~/vm/up.sh`), then finish first-run setup. Without a viewer:
   copy `crates/session/shell/tests/native_atspi_smoke.sh` into the VM and run
   it as `quickuser` with `oobe-actions`, `oobe-ready` and
   `oobe-finish-focused`, press Enter (`virsh send-key tilo-trial KEY_ENTER`),
   then `oobe-finished`.
5. Install the trial pack (above), admit the agent and allow its apps.

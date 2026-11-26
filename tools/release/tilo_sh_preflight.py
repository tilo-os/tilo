#!/usr/bin/env python3
"""tilo.sh's pods, started on this machine from tilo/infra's manifests before a release pins them.

A release changes two things at once: the images tilo/infra pins and, often, the environment its
manifests give them. Either can stop a pod, and Argo only finds out once production runs it. This
starts the pods here instead, with rootless podman and no network, so nothing reaches tilo.sh or
anything else:

  1. what production runs now: the pods of tilo/infra's committed tree (BASE, default HEAD) on a
     scratch volume, until each answers as its readiness says;
  2. the release: the pods of the working tree (the new pins and whatever else it changes) on the
     same volume, as Argo's rollout replaces the first. Each must answer again, and the Lounge
     must keep its identity (its device key and its workspace's id) across the upgrade.

With --state DIR the volume starts as a copy of DIR (the live relay's state, read-only) instead of
empty, and step 1 is skipped when --no-base says so.

Before starting anything it checks every TILO_* variable a manifest sets against the image it is
set for: a name no file in the image mentions is one the build no longer reads (a renamed or
dropped setting), which a pod would otherwise ignore or refuse.

Only the pods' own images run (registry.tilo.so/tilo/...); a sidecar of another image (Caddy, a
Python helper) is left out and named. A container that serves (one with ports) is started with its
image's entrypoint and the manifest's args: its command in the manifest is production's own wait
for the certificate and the public names, which a machine with no network never sees. Init
containers and the other containers of our images run as the manifest says.

For an unpublished release image, --archive PIN FILE uses a local single-image OCI archive
whose manifest digest matches PIN in the release tree. The live step still uses its own pins.

usage: tilo_sh_preflight.py --infra DIR --work DIR [--base REV] [--state DIR] [--no-base] [--archive PIN FILE]...
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import secrets
import shutil
import subprocess
import sys
import tarfile
import time

APPS = ("apps/tilo-sh-relay", "apps/tilo-api", "apps/tilo-web")
# The ConfigMap the relay mounts from the VM hosts' app (plans.json).
SHARED = ("apps/tilo-sh-vm-host",)
# Each Secret the pods name, by the sops file that holds it: its keys are in clear there.
SECRET_FILES = {
    "tilo-sh-relay-secrets": "clusters/prod/apps/tilo-sh-vm-host/tilo-sh-relay-secrets.sops.yaml",
}
OURS = "registry.tilo.so/tilo/"
# How a serving container with no readiness probe is known to serve: the Lounge's /mode.
READY_WITHOUT_PROBE = {"room-service": ("http", "/mode")}
READY_SECS = 180


def say(message: str) -> None:
    print(f"preflight: {message}", file=sys.stderr, flush=True)


def die(message: str) -> None:
    say(message)
    sys.exit(1)


def run(*args: str, check: bool = True, **kwargs) -> subprocess.CompletedProcess:
    kwargs.setdefault("text", True)
    return subprocess.run(args, check=check, **kwargs)


# A pod's cgroup under systemd's user manager fails to start on hosts whose user slice already
# holds one by that unit name; plain cgroupfs works wherever rootless podman does.
PODMAN = ("podman", "--cgroup-manager=cgroupfs")


def podman(*args: str, check: bool = True, capture: bool = True) -> str:
    result = run(*PODMAN, *args, check=False, capture_output=capture)
    if check and result.returncode != 0:
        die(f"podman {' '.join(args[:3])}… failed: {(result.stderr or '').strip()}")
    return (result.stdout or "").strip()


def render(tree: Path) -> dict[str, list[dict]]:
    """Every Deployment and ConfigMap the apps make, by kind."""
    import yaml

    docs: dict[str, list[dict]] = {"Deployment": [], "ConfigMap": []}
    for app in APPS + SHARED:
        out = run("kubectl", "kustomize", str(tree / app), capture_output=True).stdout
        for doc in yaml.safe_load_all(out):
            if doc and doc.get("kind") in docs:
                docs[doc["kind"]].append(doc)
    return docs


def tree_at(infra: Path, rev: str, into: Path) -> Path:
    """tilo/infra's files at REV, unpacked under INTO."""
    shutil.rmtree(into, ignore_errors=True)
    into.mkdir(parents=True)
    archive = run("git", "-C", str(infra), "archive", rev, *APPS, *SHARED, *SECRET_FILES.values(),
                  capture_output=True, text=False).stdout
    run("tar", "-x", "-C", str(into), input=archive, text=False)
    return into


class Secrets:
    """Stand-ins for the Secrets the pods mount: one file per key the sops file lists. A root is
    64 hex characters, and the same one for both steps, as the operator's is."""

    def __init__(self, infra: Path, work: Path):
        self.dir = work / "secrets"
        self.infra = infra

    def path(self, name: str) -> Path:
        import yaml

        source = SECRET_FILES.get(name)
        if source is None:
            die(f"a pod mounts the Secret {name}, which this script knows no sops file of")
        out = self.dir / name
        if out.exists():
            return out
        doc = yaml.safe_load((self.infra / source).read_text())
        keys = list((doc.get("stringData") or {}) | (doc.get("data") or {}))
        out.mkdir(parents=True)
        for key in keys:
            value = secrets.token_hex(32) if key.startswith("room-root") else "preflight"
            (out / key).write_text(value)
        return out

    def value(self, name: str, key: str) -> str:
        return (self.path(name) / key).read_text()


def archive_config(path: Path, digest: str) -> str:
    """Read the pinned manifest from a single-image OCI archive; return its immutable config ID."""
    with tarfile.open(path) as archive:
        def metadata(name: str) -> bytes:
            entry = archive.getmember(name)
            if not entry.isfile() or entry.size > 4 * 1024 * 1024:
                raise ValueError(f"invalid image metadata: {name}")
            return archive.extractfile(entry).read()

        manifests = json.loads(metadata("index.json"))["manifests"]
        if len(manifests) != 1 or manifests[0]["digest"] != digest:
            raise ValueError(f"index does not name exactly {digest}")
        manifest = metadata("blobs/" + digest.replace(":", "/"))
        if "sha256:" + hashlib.sha256(manifest).hexdigest() != digest:
            raise ValueError(f"manifest differs from {digest}")
        config = json.loads(manifest)["config"]["digest"]
        if not re.fullmatch(r"sha256:[0-9a-f]{64}", config):
            raise ValueError("invalid image config digest")
        return config.removeprefix("sha256:")


def deployment_containers(deployment: dict) -> list[dict]:
    spec = deployment["spec"]["template"]["spec"]
    return spec.get("initContainers", []) + spec["containers"]


def load_archives(archives, docs: dict[str, list[dict]]) -> dict[str, str]:
    """Resolve only the supplied release pins to local Podman image IDs."""
    local = {}
    pins = {container["image"] for deployment in docs["Deployment"] for container in deployment_containers(deployment)}
    names = set()
    for pin, name in archives:
        match = re.fullmatch(re.escape(OURS) + r"(lounge|web-entry|api)@(sha256:[0-9a-f]{64})", pin)
        if not match or pin not in pins:
            die(f"--archive must name a pinned Lounge, web-entry or API image in the release manifests: {pin}")
        if match[1] in names:
            die(f"more than one archive supplied for {match[1]}")
        names.add(match[1])
        path = Path(name).resolve()
        try:
            config = archive_config(path, match[2])
        except (OSError, tarfile.TarError, KeyError, TypeError, ValueError) as error:
            die(f"{path}: {error}")
        loaded = podman("pull", "-q", f"oci-archive:{path}").removeprefix("sha256:")
        if loaded != config:
            die(f"{path}: Podman imported {loaded}, expected config {config} for {pin}")
        local[pin] = config
    return local


def image_names(image: str, local: dict[str, str]) -> set[str]:
    """Every run of TILO_* names any file of IMAGE holds. A program's string literals sit back to
    back, so one run can hold several names: a name is read when it is part of one."""
    if image in local:
        image = local[image]
    else:
        podman("pull", "-q", image)
    script = ('mnt=$(podman image mount "$1") && trap \'podman image unmount "$1" >/dev/null\' EXIT && '
              'grep -rhoasE "TILO_[A-Z0-9_]+" "$mnt/usr" "$mnt/srv" "$mnt/etc" 2>/dev/null | sort -u')
    out = run("podman", "unshare", "sh", "-c", script, "sh", image, capture_output=True, check=False).stdout
    return set(out.split())


def unread_settings(docs: dict[str, list[dict]], local: dict[str, str]) -> list[str]:
    problems = []
    names_of: dict[str, set[str]] = {}
    for deployment in docs["Deployment"]:
        for container in deployment_containers(deployment):
            image = container["image"]
            if not image.startswith(OURS):
                continue
            if image not in names_of:
                names_of[image] = image_names(image, local)
            for env in container.get("env", []):
                name = env["name"]
                if name.startswith("TILO_") and not any(name in run_ for run_ in names_of[image]):
                    problems.append(f"{deployment['metadata']['name']}/{container['name']} sets {name}, "
                                    f"which nothing in {image.split('@')[0]} reads")
    return problems


class Pod:
    """One Deployment's pod as a podman pod with no network, its volumes under WORK."""

    def __init__(self, deployment: dict, configmaps: dict[str, dict], secrets_: Secrets, work: Path, step: str,
                 local: dict[str, str]):
        self.name = deployment["metadata"]["name"]
        self.spec = deployment["spec"]["template"]["spec"]
        self.configmaps = configmaps
        self.secrets = secrets_
        self.work = work
        self.step = step
        self.local = local
        self.pod = f"preflight-{step}-{self.name}"
        self.logs = work / "logs"
        self.logs.mkdir(parents=True, exist_ok=True)
        self.started: list[str] = []
        context = self.spec.get("securityContext", {})
        self.uid = context.get("runAsUser", 0)
        self.gid = context.get("runAsGroup", self.uid)

    def volume(self, volume: dict) -> Path:
        if "persistentVolumeClaim" in volume:
            path = self.work / "volumes" / volume["persistentVolumeClaim"]["claimName"]
        elif "secret" in volume:
            return self.secrets.path(volume["secret"]["secretName"])
        elif "configMap" in volume:
            name = volume["configMap"]["name"]
            map_ = self.configmaps.get(name)
            if map_ is None:
                die(f"{self.name} mounts the ConfigMap {name}, which no app makes")
            path = self.work / self.step / "configmaps" / name
            shutil.rmtree(path, ignore_errors=True)
            path.mkdir(parents=True)
            for key, value in (map_.get("data") or {}).items():
                (path / key).write_text(value)
            return path
        elif "emptyDir" in volume:
            path = self.work / self.step / "empty" / self.name / volume["name"]
            shutil.rmtree(path, ignore_errors=True)
        else:
            die(f"{self.name}: no stand-in for the volume {volume['name']}: {sorted(volume)}")
        path.mkdir(parents=True, exist_ok=True)
        return path

    def args(self, container: dict, serving: bool) -> list[str]:
        volumes = {v["name"]: v for v in self.spec.get("volumes", [])}
        out = ["--pod", self.pod, "--name", f"{self.pod}-{container['name']}"]
        security = container.get("securityContext", {})
        if security.get("readOnlyRootFilesystem"):
            out.append("--read-only")
        out += ["--cap-drop", "all"]
        for cap in security.get("capabilities", {}).get("add", []):
            out += ["--cap-add", cap]
        out += ["--user", f"{security.get('runAsUser', self.uid)}:{security.get('runAsGroup', self.gid)}"]
        for env in container.get("env", []):
            if "value" in env:
                value = str(env["value"])
            elif "fieldRef" in env.get("valueFrom", {}):
                value = self.pod
            elif "secretKeyRef" in env.get("valueFrom", {}):
                ref = env["valueFrom"]["secretKeyRef"]
                value = self.secrets.value(ref["name"], ref["key"])
            else:
                die(f"{self.name}/{container['name']}: no stand-in for {env['name']}")
            out += ["--env", f"{env['name']}={value}"]
        for mount in container.get("volumeMounts", []):
            host = self.volume(volumes[mount["name"]])
            if "subPath" in mount:
                host = host / mount["subPath"]
                host.mkdir(parents=True, exist_ok=True)
            mode = "ro" if mount.get("readOnly") else "rw"
            out += ["--volume", f"{host}:{mount['mountPath']}:{mode},z"]
        if not serving and container.get("command"):
            out += ["--entrypoint", json.dumps([str(c) for c in container["command"]])]
        image = container["image"]
        if image in self.local:
            out += ["--pull=never", self.local[image]]
        else:
            out.append(image)
        out += [str(a) for a in container.get("args", [])]
        return out

    def host_path(self, container: dict, path: str) -> Path | None:
        """Where PATH inside CONTAINER is on this machine, if a volume holds it."""
        volumes = {v["name"]: v for v in self.spec.get("volumes", [])}
        for mount in sorted(container.get("volumeMounts", []), key=lambda m: -len(m["mountPath"])):
            root = mount["mountPath"].rstrip("/") + "/"
            if path.startswith(root):
                host = self.volume(volumes[mount["name"]])
                if "subPath" in mount:
                    host = host / mount["subPath"]
                return host / path[len(root):]
        return None

    def certificates(self, container: dict) -> None:
        """A self-signed certificate where the manifest names one the volume lacks: production's
        tls container fetches it from Let's Encrypt, which no machine without a network reaches."""
        env = {e["name"]: e.get("value") for e in container.get("env", [])}
        for name, cert in env.items():
            if not name.endswith("_TLS_CERT") or not cert:
                continue
            key = env.get(name[: -len("CERT")] + "KEY")
            host_cert, host_key = self.host_path(container, cert), key and self.host_path(container, key)
            if not host_cert or not host_key or host_cert.exists():
                continue
            host_cert.parent.mkdir(parents=True, exist_ok=True)
            host_key.parent.mkdir(parents=True, exist_ok=True)
            run("openssl", "req", "-x509", "-newkey", "ec", "-pkeyopt", "ec_paramgen_curve:P-256", "-nodes",
                "-days", "2", "-subj", "/CN=preflight", "-keyout", str(host_key), "-out", str(host_cert),
                capture_output=True)
            say(f"{self.name}/{container['name']}: a self-signed certificate stands in for {cert}")

    def up(self) -> None:
        self.down()
        podman("pod", "create", "--name", self.pod, "--network", "none",
               "--userns", f"keep-id:uid={self.uid},gid={self.gid}")
        for container in self.spec.get("initContainers", []):
            if not container["image"].startswith(OURS):
                say(f"{self.name}: init container {container['name']} is not ours ({container['image']}): left out")
                continue
            say(f"{self.step} {self.name}: init {container['name']}")
            log = self.logs / f"{self.step}-{self.name}-{container['name']}.log"
            with log.open("w") as out:
                result = run(*PODMAN, "run", "--rm", *self.args(container, serving=False),
                             check=False, stdout=out, stderr=subprocess.STDOUT)
            if result.returncode != 0:
                die(f"{self.name}: init container {container['name']} exited {result.returncode}:\n{log.read_text()[-3000:]}")
        for container in self.spec["containers"]:
            if not container["image"].startswith(OURS):
                say(f"{self.name}: {container['name']} is not ours ({container['image'].split('@')[0]}): left out")
                continue
            serving = bool(container.get("ports"))
            self.certificates(container)
            podman("run", "-d", *self.args(container, serving=serving))
            self.started.append(container["name"])

    def pid(self) -> str:
        infra = podman("pod", "inspect", "-f", "{{.InfraContainerID}}", self.pod)
        return podman("inspect", "-f", "{{.State.Pid}}", infra)

    def get(self, port: int, path: str, method: str = "GET") -> tuple[int, str]:
        result = run(*PODMAN, "unshare", "nsenter", f"--net=/proc/{self.pid()}/ns/net",
                     "curl", "-s", "-m", "5", "-X", method, "-w", "\n%{http_code}", f"http://127.0.0.1:{port}{path}",
                     capture_output=True, check=False)
        body, _, code = result.stdout.rpartition("\n")
        return (int(code) if code.isdigit() else 0), body

    def ready(self) -> dict[str, str]:
        """Each serving container's answer once it is ready; the pod's logs on a failure."""
        answers = {}
        for container in self.spec["containers"]:
            if container["name"] not in self.started or not container.get("ports"):
                continue
            ports = {p.get("name"): p["containerPort"] for p in container["ports"]}
            probe = container.get("readinessProbe", {}).get("httpGet")
            if probe:
                port, path = probe["port"], probe.get("path", "/")
            elif container["name"] in READY_WITHOUT_PROBE:
                port, path = READY_WITHOUT_PROBE[container["name"]]
            else:
                continue
            port = ports.get(port, port)
            deadline = time.monotonic() + READY_SECS
            while True:
                state = podman("inspect", "-f", "{{.State.Status}}", f"{self.pod}-{container['name']}", check=False)
                if state != "running":
                    self.keep_logs()
                    log = self.logs / f"{self.step}-{self.name}-{container['name']}.log"
                    die(f"{self.step} {self.name}/{container['name']} stopped ({state}):\n{log.read_text()[-4000:]}")
                code, body = self.get(int(port), path)
                if code == 200:
                    answers[container["name"]] = body.strip()
                    say(f"{self.step} {self.name}/{container['name']}: {path} answers {body.strip()[:200]}")
                    break
                if time.monotonic() > deadline:
                    self.keep_logs()
                    die(f"{self.step} {self.name}/{container['name']}: {path} did not answer 200 in {READY_SECS} s "
                        f"(last {code}); its log is {self.logs}")
                time.sleep(2)
        # A container of ours that serves nothing still has to keep running: one that exits makes
        # the pod unready in production.
        for name in self.started:
            state = podman("inspect", "-f", "{{.State.Status}}", f"{self.pod}-{name}", check=False)
            if state != "running":
                self.keep_logs()
                log = self.logs / f"{self.step}-{self.name}-{name}.log"
                die(f"{self.step} {self.name}/{name} is {state}, not running:\n{log.read_text()[-3000:]}")
        return answers

    def keep_logs(self) -> None:
        for name in self.started:
            result = run(*PODMAN, "logs", f"{self.pod}-{name}", check=False, capture_output=True)
            if result.returncode == 0:
                (self.logs / f"{self.step}-{self.name}-{name}.log").write_text(result.stdout + result.stderr)

    def down(self) -> None:
        self.keep_logs()
        podman("pod", "rm", "-f", "-t", "20", self.pod, check=False)


def identity(volumes: Path) -> tuple[set[str], set[str]]:
    """The Lounge's identity on the volume: the digests of the device keys it holds and the ids
    of the workspaces it names, wherever the builds that wrote them keep them (a release that
    starts its state afresh leaves the earlier one where it lay)."""
    keys = {hashlib.sha256(key.read_bytes()).hexdigest()[:16] for key in volumes.rglob("host-device.key")}
    workspaces = {id_file.read_text().strip() for id_file in volumes.rglob("workspace-id")}
    return keys, workspaces


def documents(volumes: Path) -> int:
    """How many files of workspace documents the volume holds: what the release must read back."""
    return sum(1 for path in volumes.rglob("*") if path.is_file()
               and (path.suffix in (".delta", ".pack", ".automerge") or path.name == ".journal"))


def shape(value):
    """VALUE's keys, nested, with what they hold left out: the form of a JSON document. A list is
    only a list: the addresses a host reaches depend on the network it starts in."""
    if isinstance(value, dict):
        return {key: shape(item) for key, item in value.items()}
    return [] if isinstance(value, list) else None


def env_of(docs: dict[str, list[dict]], deployment: str, container: str, name: str) -> str | None:
    for d in docs["Deployment"]:
        if d["metadata"]["name"] != deployment:
            continue
        for c in d["spec"]["template"]["spec"]["containers"]:
            if c["name"] == container:
                return next((e.get("value") for e in c.get("env", []) if e["name"] == name), None)
    return None


def host_info(pod: Pod) -> dict:
    """What the running Lounge's server wrote for the web entry's TILO_WEB_HOME_HOST_INFO."""
    container = f"{pod.pod}-room-service"
    result = run(*PODMAN, "exec", container, "sh", "-c",
                 'cat "${TILO_WORKSPACES_STATE:-/run/tilo-workspaces}/$TILO_WORKSPACES_PERSISTENT/server/server/web-host.json"',
                 capture_output=True, check=False)
    if result.returncode != 0:
        die(f"the Lounge's server wrote no web-host.json, which the web entry's and the API's host lines "
            f"are copied from: {result.stderr.strip()[-2000:]}")
    return json.loads(result.stdout.strip().splitlines()[-1])


def step(name: str, docs: dict[str, list[dict]], secrets_: Secrets, work: Path, real_state: bool,
         local: dict[str, str]) -> None:
    configmaps = {c["metadata"]["name"]: c for c in docs["ConfigMap"]}
    pods = [Pod(d, configmaps, secrets_, work, name, local) for d in docs["Deployment"]
            if any(c["image"].startswith(OURS) for c in d["spec"]["template"]["spec"]["containers"])
            and d["metadata"]["name"].startswith(("tilo-sh-relay", "tilo-api", "tilo-web"))]
    try:
        for pod in pods:
            pod.up()
        for pod in pods:
            pod.ready()
        relay = next((p for p in pods if p.name == "tilo-sh-relay"), None)
        pinned = env_of(docs, "tilo-web", "web-entry", "TILO_WEB_HOME_HOST_INFO")
        if relay and pinned:
            printed, pinned = host_info(relay), json.loads(pinned)
            if shape(printed) != shape(pinned):
                die(f"{name}: the web entry's TILO_WEB_HOME_HOST_INFO is not in the form this server prints:\n"
                    f"  pinned  {json.dumps(shape(pinned))}\n  printed {json.dumps(shape(printed))}")
            if real_state and printed != pinned:
                die(f"{name}: on the relay's own state the server is not the host the web entry names:\n"
                    f"  pinned  {json.dumps(pinned)}\n  printed {json.dumps(printed)}")
            say(f"{name}: the web entry's host line has the form the server prints"
                + (", and names this host" if real_state else ""))
        entry = env_of(docs, "tilo-web", "web-entry", "TILO_WEB_HOME_ENTRY")
        if relay and entry and real_state:
            # The entry names the Lounge's workspace by id: a Lounge started afresh has a new
            # one, and a web entry left on the old one turns every newcomer away.
            code, body = relay.get(9780, "/peer/v1/directory", method="POST")
            listed = {w.get("entry", "").strip("/").rsplit("/", 1)[-1] for w in json.loads(body).get("workspaces", [])} if code == 200 else set()
            named = entry.rstrip("/").rsplit("/", 1)[-1]
            if named not in listed:
                die(f"{name}: the web entry's TILO_WEB_HOME_ENTRY names workspace {named}, which this server "
                    f"does not list (it lists {sorted(listed)}): set it to the Lounge's entry")
            say(f"{name}: the web entry's TILO_WEB_HOME_ENTRY is the Lounge's entry ({named})")
    finally:
        for pod in pods:
            pod.down()


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--infra", type=Path, required=True, help="tilo/infra's working tree, the release in it")
    parser.add_argument("--work", type=Path, required=True, help="where the volumes and logs are kept")
    parser.add_argument("--base", default="HEAD", help="the tilo/infra revision production runs (HEAD)")
    parser.add_argument("--state", type=Path, help="a copy of the relay's state volume to start on")
    parser.add_argument("--no-base", action="store_true", help="with --state: start the release on it directly")
    parser.add_argument("--archive", nargs=2, action="append", default=[], metavar=("PIN", "FILE"),
                        help="read the release manifest's exact image@sha256:digest from a local OCI archive (repeatable)")
    a = parser.parse_args()
    infra, work = a.infra.resolve(), a.work.resolve()
    shutil.rmtree(work, ignore_errors=True)
    work.mkdir(parents=True)
    secrets_ = Secrets(infra, work)

    release = render(infra)
    local = load_archives(a.archive, release)
    problems = unread_settings(release, local)
    if problems:
        die("the release's manifests set what its images do not read:\n  " + "\n  ".join(problems))
    say("every TILO_* setting the manifests give is one its image reads")

    volume = work / "volumes" / "tilo-sh-relay-state"
    if a.state:
        say(f"the relay's volume starts as a copy of {a.state}")
        shutil.copytree(a.state, volume, symlinks=True)
    if not a.no_base:
        base = render(tree_at(infra, a.base, work / "base-tree"))
        step("live", base, secrets_, work, bool(a.state), {})
    (keys, workspaces) = identity(work / "volumes")
    written = documents(work / "volumes")
    if not a.no_base and not written:
        die("the live build left no workspace documents on the volume: the upgrade would read nothing it wrote")
    step("release", release, secrets_, work, bool(a.state), local)
    (keys_after, workspaces_after) = identity(work / "volumes")
    # The device key is the Lounge's address: the web entry dials it and the VM hosts take work
    # from it. The Lounge's workspace and what it holds are the people's: a release that starts
    # them afresh, or moves them aside, loses them.
    if keys and keys_after != keys:
        die(f"the Lounge's device key changed across the upgrade: before {sorted(keys)}, after {sorted(keys_after)}")
    if workspaces_after != workspaces:
        die(f"the Lounge's workspace changed across the upgrade: before {sorted(workspaces)}, after {sorted(workspaces_after)}")
    say(f"the release serves on the volume the live pods left ({written} document files), with the same "
        f"device key ({sorted(keys_after)}) and workspace ({sorted(workspaces_after)})")


if __name__ == "__main__":
    main()

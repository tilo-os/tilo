#!/usr/bin/env python3
"""The OS image as layers: fragments of an OCI image that layer actions make, and the
images composed from them, without a container runtime.

A *fragment* is the layers one layer action added, as a directory:

    fragment.json   {"action", "inputs": {path: sha256}, "inputsDigest",
                     "layers": [{mediaType, digest, size, diffID}], "config"?}
    blobs/sha256/   the layers' blobs

A full OCI layout (index.json, blobs/) stands for the fragment of all its image's
layers, so the pinned base is a fragment as it comes from the registry.

    oci_layers.py inputs GLOB...                     the inputs' digests, as JSON
    oci_layers.py take PARENT BUILT OUT ACTION INPUTS
                                                     the layers the layout BUILT has over
                                                     the layout PARENT (`-` for none), as a
                                                     fragment
    oci_layers.py compose OUT FRAGMENT...            an image of the fragments' layers, in
                                                     order, as a layout in OUT/oci, its
                                                     digest in OUT/digest
    oci_layers.py index OUT LAYOUT...                one multi-architecture index of the
                                                     layouts' images, as a layout
    oci_layers.py attach LAYOUT DIGEST PAYLOAD SIG[,BUNDLE[,CERT]]...
                                                     add the cosign signatures of DIGEST
                                                     (one of the layout's images) to the
                                                     layout, as the manifest cosign reads
                                                     under sha256-<hex>.sig, which t's
                                                     server-side publishing pushes after
                                                     the image
    oci_layers.py diff OLD NEW [REPORT] [--strict]   which layers of NEW are new, which
                                                     action made each and which of its
                                                     inputs changed, and, for a layer whose
                                                     action's inputs did not change, the
                                                     files that differ: a nondeterminism;
                                                     --strict fails on any

Composing writes a manifest and a configuration and links the blobs it was given:
it takes seconds whatever the image's size, and the same fragments give the same
digest. Every layer's descriptor carries the action that made it and the digest of
that action's inputs (org.tilo.layer.action, org.tilo.layer.inputs), and its
configuration's history says the same, so two images tell which layers differ and
why.
"""

import glob
import gzip
import hashlib
import io
import json
import os
import shutil
import sys
import tarfile

INDEX = "application/vnd.oci.image.index.v1+json"
MANIFEST = "application/vnd.oci.image.manifest.v1+json"
CONFIG = "application/vnd.oci.image.config.v1+json"
DOCKER_MANIFEST = "application/vnd.docker.distribution.manifest.v2+json"
DOCKER_LIST = "application/vnd.docker.distribution.manifest.list.v2+json"
ACTION = "org.tilo.layer.action"
INPUTS = "org.tilo.layer.inputs"


def die(msg):
    print(f"oci_layers: {msg}", file=sys.stderr)
    sys.exit(1)


def sha256_bytes(data):
    return "sha256:" + hashlib.sha256(data).hexdigest()


def sha256_file(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return "sha256:" + h.hexdigest()


def blob_path(root, digest):
    algo, hexd = digest.split(":", 1)
    if algo != "sha256" or len(hexd) != 64:
        die(f"{digest} is not a sha256 digest")
    return os.path.join(root, "blobs", algo, hexd)


def read_blob(root, digest):
    with open(blob_path(root, digest), "rb") as f:
        data = f.read()
    if sha256_bytes(data) != digest:
        die(f"{root}: blob {digest} does not hold what its name says")
    return data


def put_blob(root, data):
    digest = sha256_bytes(data)
    path = blob_path(root, digest)
    os.makedirs(os.path.dirname(path), exist_ok=True)
    if not os.path.exists(path):
        with open(path, "wb") as f:
            f.write(data)
    return digest, len(data)


def link_blob(src_root, dst_root, digest):
    src, dst = blob_path(src_root, digest), blob_path(dst_root, digest)
    os.makedirs(os.path.dirname(dst), exist_ok=True)
    if os.path.exists(dst):
        return
    try:
        os.link(src, dst)
    except OSError:
        shutil.copyfile(src, dst)


def canonical(obj):
    return json.dumps(obj, separators=(",", ":"), sort_keys=True).encode()


def oci_arch(machine):
    return {"x86_64": "amd64", "aarch64": "arm64"}.get(machine, machine)


def image_of_layout(root, arch=None):
    """The manifest and config of the layout's image for `arch` (this machine's by default)."""
    arch = oci_arch(arch or os.uname().machine)
    with open(os.path.join(root, "index.json")) as f:
        doc = json.load(f)
    for _ in range(4):
        if "config" in doc:
            break
        manifests = [m for m in doc.get("manifests", []) if not m.get("annotations", {}).get(
            "org.opencontainers.image.ref.name", "").startswith("sha256-")]
        chosen = [m for m in manifests if m.get("platform", {}).get("architecture") == arch]
        if not chosen and len(manifests) == 1:
            chosen = manifests
        if len(chosen) != 1:
            die(f"{root} holds no single image for linux/{arch}")
        doc = json.loads(read_blob(root, chosen[0]["digest"]))
    config = json.loads(read_blob(root, doc["config"]["digest"]))
    return doc, config


def load_fragment(path, arch=None):
    """A fragment directory, or a whole layout standing for the fragment of its layers."""
    meta = os.path.join(path, "fragment.json")
    if os.path.exists(meta):
        with open(meta) as f:
            frag = json.load(f)
        frag["root"] = path
        return frag
    manifest, config = image_of_layout(path, arch)
    diff_ids = config.get("rootfs", {}).get("diff_ids", [])
    if len(diff_ids) != len(manifest["layers"]):
        die(f"{path}: {len(manifest['layers'])} layers but {len(diff_ids)} diff_ids")
    layers = []
    for layer, diff_id in zip(manifest["layers"], diff_ids):
        layers.append({"mediaType": layer.get("mediaType", ""), "digest": layer["digest"],
                       "size": layer["size"], "diffID": diff_id,
                       "annotations": layer.get("annotations", {})})
    return {"root": path, "action": os.path.basename(path.rstrip("/")), "inputsDigest": "",
            "inputs": {}, "layers": layers, "config": config}


def cmd_inputs(globs):
    """The digest of every file the globs match, relative to the repository: what an action
    is keyed by, kept with its layers so a later image can say which input changed."""
    files = set()
    for pattern in globs:
        for p in glob.glob(pattern, recursive=True):
            if os.path.isfile(p):
                files.add(os.path.normpath(p))
    inputs = {p: sha256_file(p) for p in sorted(files)}
    print(json.dumps({"inputs": inputs, "inputsDigest": sha256_bytes(canonical(inputs))}))


def cmd_take(parent, built, out, action, inputs_file):
    """The layers BUILT added over PARENT. BUILT was built FROM PARENT, so its diff_ids start
    with PARENT's; anything else means the action didn't build on what it was given."""
    b_manifest, b_config = image_of_layout(built)
    # `-`: built over nothing (a scratch stage), so every layer is its own.
    p_ids = [] if parent == "-" else image_of_layout(parent)[1]["rootfs"]["diff_ids"]
    b_ids = b_config["rootfs"]["diff_ids"]
    if b_ids[: len(p_ids)] != p_ids:
        die(f"{built} is not built on {parent}: its first {len(p_ids)} layers differ")
    with open(inputs_file) as f:
        inputs = json.load(f)
    os.makedirs(out, exist_ok=True)
    layers = []
    for layer, diff_id in zip(b_manifest["layers"][len(p_ids):], b_ids[len(p_ids):]):
        link_blob(built, out, layer["digest"])
        layers.append({"mediaType": layer.get("mediaType", ""), "digest": layer["digest"],
                       "size": layer["size"], "diffID": diff_id})
    frag = {"action": action, "inputs": inputs["inputs"], "inputsDigest": inputs["inputsDigest"],
            "layers": layers, "config": b_config}
    with open(os.path.join(out, "fragment.json"), "w") as f:
        json.dump(frag, f, indent=1, sort_keys=True)
    size = sum(l["size"] for l in layers)
    print(f"{action}: {len(layers)} layers, {size / 1048576:.1f} MiB, inputs {inputs['inputsDigest'][:19]}")


def compose(out, fragments, epoch):
    """The image of the fragments' layers in order, as an OCI layout in OUT/oci; its manifest
    descriptor. The configuration is the topmost fragment's (its environment, labels,
    command), with the layers' diff_ids and one history entry per layer saying which action
    made it."""
    frags = [load_fragment(f) for f in fragments]
    config = next((f["config"] for f in reversed(frags) if f.get("config")), None)
    if config is None:
        die("no fragment carries an image configuration")
    config = json.loads(json.dumps(config))
    root = os.path.join(out, "oci")
    shutil.rmtree(root, ignore_errors=True)
    os.makedirs(os.path.join(root, "blobs", "sha256"))
    # Dated as the topmost layer action dated its image (SOURCE_DATE_EPOCH, by buildah's
    # --source-date-epoch), so composing never puts the time it ran into the image.
    created = config.get("created") or epoch_time(epoch)
    layers, diff_ids, history = [], [], []
    for frag in frags:
        for layer in frag["layers"]:
            link_blob(frag["root"], root, layer["digest"])
            annotations = dict(layer.get("annotations") or {})
            if frag.get("inputsDigest"):
                annotations[ACTION] = frag["action"]
                annotations[INPUTS] = frag["inputsDigest"]
            desc = {"mediaType": layer["mediaType"] or "application/vnd.oci.image.layer.v1.tar+gzip",
                    "digest": layer["digest"], "size": layer["size"]}
            if annotations:
                desc["annotations"] = dict(sorted(annotations.items()))
            layers.append(desc)
            diff_ids.append(layer["diffID"])
            history.append({"created": created,
                            "created_by": f"{frag['action']} (inputs {frag.get('inputsDigest') or 'pinned'})"})
    config["rootfs"] = {"type": "layers", "diff_ids": diff_ids}
    config["history"] = history
    config["created"] = created
    config_digest, config_size = put_blob(root, canonical(config))
    manifest = {"schemaVersion": 2, "mediaType": MANIFEST,
                "config": {"mediaType": CONFIG, "digest": config_digest, "size": config_size},
                "layers": layers}
    digest, size = put_blob(root, canonical(manifest))
    desc = {"mediaType": MANIFEST, "digest": digest, "size": size,
            "platform": {"architecture": config.get("architecture", oci_arch(os.uname().machine)),
                         "os": config.get("os", "linux")}}
    write_layout(root, desc)
    with open(os.path.join(out, "digest"), "w") as f:
        f.write(digest + "\n")
    print(f"composed {digest}: {len(layers)} layers from {len(frags)} fragments")
    return desc


def epoch_time(epoch):
    import datetime
    return datetime.datetime.fromtimestamp(int(epoch), datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def write_layout(root, desc, name="tilo"):
    with open(os.path.join(root, "oci-layout"), "w") as f:
        f.write('{"imageLayoutVersion":"1.0.0"}')
    top = dict(desc)
    top["annotations"] = {"org.opencontainers.image.ref.name": name}
    with open(os.path.join(root, "index.json"), "w") as f:
        json.dump({"schemaVersion": 2, "manifests": [top]}, f)


def cmd_index(out, layouts):
    """One index of the layouts' images (one per architecture), as a layout in OUT/oci."""
    root = os.path.join(out, "oci")
    shutil.rmtree(root, ignore_errors=True)
    os.makedirs(os.path.join(root, "blobs", "sha256"))
    manifests = []
    for layout in layouts:
        with open(os.path.join(layout, "index.json")) as f:
            [desc] = [m for m in json.load(f)["manifests"]]
        manifest = json.loads(read_blob(layout, desc["digest"]))
        config = json.loads(read_blob(layout, manifest["config"]["digest"]))
        for d in [desc["digest"], manifest["config"]["digest"]] + [l["digest"] for l in manifest["layers"]]:
            link_blob(layout, root, d)
        manifests.append({"mediaType": MANIFEST, "digest": desc["digest"], "size": desc["size"],
                          "platform": {"architecture": config["architecture"], "os": config["os"]}})
    arches = [m["platform"]["architecture"] for m in manifests]
    if len(set(arches)) != len(arches):
        die(f"two images of one architecture: {arches}")
    index = {"schemaVersion": 2, "mediaType": INDEX, "manifests": manifests}
    digest, size = put_blob(root, canonical(index))
    write_layout(root, {"mediaType": INDEX, "digest": digest, "size": size})
    with open(os.path.join(out, "digest"), "w") as f:
        f.write(digest + "\n")
    print(f"index {digest}: {', '.join(arches)}")


def layer_files(root, layer):
    """A layer's files: path -> (content digest or link target, mode, owner, mtime)."""
    with open(blob_path(root, layer["digest"]), "rb") as f:
        data = f.read()
    if data[:2] == b"\x1f\x8b":
        data = gzip.decompress(data)
    elif data[:4] == b"\x28\xb5\x2f\xfd":
        return None
    files = {}
    with tarfile.open(fileobj=io.BytesIO(data)) as tar:
        for m in tar:
            if m.isfile():
                content = hashlib.sha256(tar.extractfile(m).read()).hexdigest()
            else:
                content = m.linkname or m.type.decode(errors="replace")
            files[m.name] = (content, m.mode, (m.uid, m.gid), m.mtime)
    return files


def cmd_diff(old, new, out=None):
    """Say which layers of NEW are not in OLD, and why: the action whose inputs changed (and
    which inputs), or, where its inputs are the same, the files that differ and whether in
    content or only in metadata."""
    o_manifest, _ = image_of_layout(old)
    n_manifest, _ = image_of_layout(new)
    have = {l["digest"] for l in o_manifest["layers"]}
    by_action = {}
    for l in o_manifest["layers"]:
        a = l.get("annotations", {}).get(ACTION)
        if a:
            by_action.setdefault(a, []).append(l)
    sent = 0
    nondeterministic = 0
    lines = []
    import builtins
    real_print = builtins.print

    def print(*a):  # noqa: A001 - every line goes to the report too
        line = " ".join(str(x) for x in a)
        lines.append(line)
        real_print(line)

    for i, l in enumerate(n_manifest["layers"]):
        if l["digest"] in have:
            continue
        sent += l["size"]
        a = l.get("annotations", {}).get(ACTION, "?")
        inputs = l.get("annotations", {}).get(INPUTS, "")
        olds = by_action.get(a, [])
        before = olds[0].get("annotations", {}).get(INPUTS, "") if olds else ""
        if not olds:
            print(f"layer {i} ({l['size'] / 1048576:.1f} MiB): new action {a}")
        elif before != inputs:
            print(f"layer {i} ({l['size'] / 1048576:.1f} MiB): {a} ran again, its inputs changed ({before[:19]} -> {inputs[:19]})")
        else:
            nondeterministic += 1
            print(f"layer {i} ({l['size'] / 1048576:.1f} MiB): {a}'s inputs are the same, yet its layer differs: nondeterministic")
            pos = [j for j, x in enumerate(n_manifest["layers"]) if x.get("annotations", {}).get(ACTION) == a].index(i)
            if pos < len(olds):
                was, now = layer_files(old, olds[pos]), layer_files(new, l)
                if was is None or now is None:
                    print("  (zstd layers are not read here)")
                    continue
                for path in sorted(set(was) | set(now)):
                    w, n = was.get(path), now.get(path)
                    if w == n:
                        continue
                    if w is None or n is None:
                        print(f"  {'only in the new' if w is None else 'only in the old'}: {path}")
                    elif w[0] != n[0]:
                        print(f"  content: {path}")
                    else:
                        what = [k for k, x, y in (("mode", w[1], n[1]), ("owner", w[2], n[2]), ("time", w[3], n[3])) if x != y]
                        print(f"  {', '.join(what)} only: {path}")
    print(f"an update from the old image downloads {sent / 1048576:.1f} MiB")
    if out:
        with open(out, "w") as f:
            f.write("\n".join(lines) + "\n")
    return nondeterministic


def cmd_attach(root, digest, payload_file, signatures):
    """The signatures of DIGEST over the simple-signing PAYLOAD, as cosign stores them: one
    manifest whose layers are the payload, once per signature, each annotated with its
    signature (and its transparency log bundle and certificate where it has them), put into
    the layout's index.json under the tag cosign looks for."""
    import base64
    with open(payload_file, "rb") as f:
        payload = f.read()
    payload_digest, payload_size = put_blob(root, payload)
    layers = []
    for entry in signatures:
        sig, bundle, cert = (entry.split(",") + ["", ""])[:3]
        with open(sig) as f:
            annotations = {"dev.cosignproject.cosign/signature": f.read().strip()}
        if bundle:
            with open(bundle) as f:
                annotations["dev.sigstore.cosign/bundle"] = json.dumps(json.load(f), separators=(",", ":"))
        if cert:
            with open(cert) as f:
                annotations["dev.sigstore.cosign/certificate"] = f.read()
        layers.append({"mediaType": "application/vnd.dev.cosign.simplesigning.v1+json",
                       "size": payload_size, "digest": payload_digest, "annotations": annotations})
    config = {"architecture": "", "os": "", "config": {}, "created": "0001-01-01T00:00:00Z",
              "rootfs": {"type": "layers", "diff_ids": [payload_digest] * len(layers)}}
    config_digest, config_size = put_blob(root, canonical(config))
    manifest = {"schemaVersion": 2, "mediaType": MANIFEST,
                "config": {"mediaType": CONFIG, "digest": config_digest, "size": config_size},
                "layers": layers}
    m_digest, m_size = put_blob(root, canonical(manifest))
    tag = "sha256-" + digest.split(":", 1)[1] + ".sig"
    path = os.path.join(root, "index.json")
    with open(path) as f:
        index = json.load(f)
    index["manifests"] = [m for m in index["manifests"]
                          if m.get("annotations", {}).get("org.opencontainers.image.ref.name") != tag]
    index["manifests"].append({"mediaType": MANIFEST, "digest": m_digest, "size": m_size,
                               "annotations": {"org.opencontainers.image.ref.name": tag}})
    with open(path, "w") as f:
        json.dump(index, f)
    print(f"attached {len(layers)} signatures of {digest} as {tag}")


def main(argv):
    if not argv:
        die(__doc__)
    cmd, args = argv[0], argv[1:]
    if cmd == "inputs":
        cmd_inputs(args)
    elif cmd == "take" and len(args) == 5:
        cmd_take(*args)
    elif cmd == "compose" and len(args) >= 2:
        compose(args[0], args[1:], os.environ.get("SOURCE_DATE_EPOCH", "0"))
    elif cmd == "index" and len(args) >= 2:
        cmd_index(args[0], args[1:])
    elif cmd == "attach" and len(args) >= 4:
        cmd_attach(args[0], args[1], args[2], args[3:])
    elif cmd == "diff" and len(args) in (2, 3, 4):
        strict = "--strict" in args
        rest = [a for a in args if a != "--strict"]
        found = cmd_diff(*rest)
        if strict and found:
            die(f"{found} layers changed although the inputs of the actions that made them did not")
    else:
        die(__doc__)


if __name__ == "__main__":
    main(sys.argv[1:])

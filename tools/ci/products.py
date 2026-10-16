#!/usr/bin/env python3
"""The publishing chains of t.toml, generated from tools/ci/products.toml.

Every product Tilo publishes as a container image runs the same chain of checks, per
architecture where it is built natively (docs/publishing-on-t.md, "Products"):

  image-<name>-context  prepares the build context (optional), no secrets
  image-<name>[-<arch>] builds the image into an OCI archive and tests it, no secrets
  image-<name>          joins the architectures into one image index (`index = true`)
  sign-<name>           signs the digest the build produced, on the release references only
  publish-<name>        pushes it: development tags from main, the signed release from stable
                        or release tags, in one check whose signature is an optional need

A product in products.toml is a few lines of data; this module turns it into those checks.
t_toml.py writes them into t.toml between markers and the policy tests fail while they are
out of date.
"""
from __future__ import annotations

import json
from pathlib import Path
import re
import tomllib
from typing import Any

ROOT = Path(__file__).resolve().parents[2]
MANIFEST = ROOT / "tools" / "ci" / "products.toml"
PUBLISH = "tools/ci/publish"
PLATFORMS = {"x86_64": "linux/amd64", "aarch64": "linux/arm64"}
# What every build, signing and publishing check reads besides the product's own inputs.
BUILD_INPUTS = [f"{PUBLISH}/lib.sh", f"{PUBLISH}/oci-build.sh", f"{PUBLISH}/oci-run.sh"]
COSIGN_INPUTS = ["os/dev/install-cached-cosign.sh", "os/dev/cache-github-assets.sh",
                 "os/external-assets/github-assets.tsv", "keys/cosign.pub"]
SIGN_INPUTS = [f"{PUBLISH}/lib.sh", f"{PUBLISH}/oci-sign.sh", "os/dev/configure-cosign-kms.sh", *COSIGN_INPUTS]
PUBLISH_INPUTS = [f"{PUBLISH}/lib.sh", f"{PUBLISH}/oci-publish.sh", *COSIGN_INPUTS]
# Signing and publishing read only what the build produced, so they run on any architecture (as
# micro-VM steps, since they hold credentials, booting the guest of the node they land on).
CREDENTIAL_REQUIRES = ["arch=any"]
# Keys of a product table, and of its `context` and `build` tables, that are not check fields.
PRODUCT_KEYS = {"name", "about", "repository", "arches", "index", "dev", "release", "release_on",
                "tlog", "context", "build", "gates", "anonymous", "after_publish", "publish",
                "dir", "out", "dispatch"}
STEP_KEYS = {"run", "test", "containerfile", "context", "build_args", "name", "about"}
# Fields the generator writes itself, so a product can't set them by accident.
OWN_FIELDS = {"name", "run", "needs", "optional_needs", "on", "id_token", "secrets"}
# The order fields appear in, as the hand-written checks of t.toml have them.
FIELD_ORDER = ["name", "image", "run", "inputs", "cargo_packages", "cargo_dev_dependencies", "needs",
               "optional_needs", "outputs", "pointers", "id_token", "env", "git", "requires",
               "isolation", "privileged", "writable_sources", "cost", "cpus", "memory", "timeout_secs", "on",
               "default"]


class ProductError(ValueError):
    pass


def load(path: Path = MANIFEST) -> dict[str, Any]:
    return tomllib.loads(path.read_text(encoding="utf-8"))


def value(v: Any) -> str:
    """A TOML value as t.toml writes it."""
    if isinstance(v, bool):
        return "true" if v else "false"
    if isinstance(v, int):
        return str(v)
    if isinstance(v, str):
        if "\n" in v:
            # A literal string where escapes would otherwise apply, as in shell commands.
            if "\\" in v or '"""' in v:
                if "\'\'\'" in v:
                    raise ProductError(f"no multi-line TOML string for {v!r}")
                return "\'\'\'\n" + v + "\'\'\'"
            return '"""\n' + v + '"""'
        if '"' in v and "'" not in v:
            return "'" + v + "'"
        return json.dumps(v)
    if isinstance(v, list):
        return "[" + ", ".join(value(x) for x in v) + "]"
    if isinstance(v, dict):
        return "{ " + ", ".join(f"{key(k)} = {value(x)}" for k, x in v.items()) + " }"
    raise ProductError(f"no TOML for {v!r}")


def key(k: str) -> str:
    return k if re.fullmatch(r"[A-Za-z0-9_-]+", k) else json.dumps(k)


def publish_scripts(text: str, seen: set[str] | None = None) -> set[str]:
    """The tools/ci/publish scripts a command runs, and those they run or source in turn."""
    seen = set() if seen is None else seen
    names = [f"{PUBLISH}/{n}" for n in re.findall(r"tools/ci/publish/([\w.-]+\.(?:sh|py))", text)]
    names += [f"{PUBLISH}/{n}" for n in
              re.findall(r'\$\(dirname -- "\$\{BASH_SOURCE\[0\]\}"\)/([\w.-]+\.(?:sh|py))', text)]
    for name in names:
        if name not in seen and (ROOT / name).is_file():
            seen.add(name)
            publish_scripts((ROOT / name).read_text(encoding="utf-8"), seen)
    return seen


def check(fields: dict[str, Any], comment: str | None = None) -> str:
    # Every check reads the publishing scripts it runs, so a change to one reruns exactly those.
    scripts = sorted(publish_scripts(fields.get("run", "")) - set(fields.get("inputs", [])))
    if scripts:
        fields = {**fields, "inputs": [*fields.get("inputs", []), *scripts]}
    order = {name: i for i, name in enumerate(FIELD_ORDER)}
    lines = [f"# {line}".rstrip() for line in comment.strip().splitlines()] if comment else []
    lines.append("[[check]]")
    tables = []
    for name in sorted(fields, key=lambda n: (order.get(n, len(order)), n)):
        v = fields[name]
        if name == "dispatch_inputs":
            tables.append(f"\n[check.dispatch_inputs]\n" + "".join(f"{key(k)} = {value(x)}\n" for k, x in v.items()))
            continue
        lines.append(f"{key(name)} = {value(v)}")
    return "\n".join(lines) + "\n" + "".join(tables)


def fill(v: Any, values: dict[str, str]) -> Any:
    """Replace {dir}, {out}, {arch}, {platform} and {name} in strings, leaving others ({commit})."""
    if isinstance(v, str):
        return re.sub(r"\{(dir|out|arch|platform|name)\}", lambda m: values[m[1]], v)
    if isinstance(v, list):
        return [fill(x, values) for x in v]
    if isinstance(v, dict):
        return {k: fill(x, values) for k, x in v.items()}
    return v


def dedupe(values: list[str]) -> list[str]:
    return list(dict.fromkeys(values))


class Product:
    def __init__(self, data: dict[str, Any], defaults: dict[str, Any]):
        unknown = set(data) - PRODUCT_KEYS
        if unknown:
            raise ProductError(f"product {data.get('name')!r}: unknown keys {sorted(unknown)}")
        self.data = data
        self.defaults = defaults
        self.name: str = data["name"]
        self.repository: str = data["repository"]
        self.arches: list[str] = data.get("arches", ["x86_64"])
        for arch in self.arches:
            if arch not in PLATFORMS:
                raise ProductError(f"{self.name}: unknown architecture {arch}")
        self.index: bool = data.get("index", False)
        if self.index and len(self.arches) < 2:
            raise ProductError(f"{self.name}: an index joins several architectures")
        self.dev: list[str] = data.get("dev", [])
        self.release: list[str] = data.get("release", [])
        self.release_on: list[str] = data.get("release_on", ["stable"])
        self.dir: str = data.get("dir", f"/target/publish/{self.name}")
        self.gates: list[str] = data.get("gates", [])
        # Development tags are pushed when main changes, or with `dispatch = true` only when
        # someone asks (`t dispatch publish-<name> --ref main`): for builds too costly to run
        # on every change of their inputs.
        self.dev_on: list[str] = ["dispatch"] if data.get("dispatch") else ["main"]
        for field in ("anonymous", "after_publish", "publish", "tlog"):
            if field in data and not (self.dev or self.release):
                raise ProductError(f"{self.name}: {field} without tags to publish (dev, release)")

    @property
    def multi(self) -> bool:
        return len(self.arches) > 1

    def build_name(self, arch: str) -> str:
        return f"image-{self.name}-{arch}" if self.multi else f"image-{self.name}"

    def out(self, arch: str) -> str:
        default = f"{self.dir}/{arch}" if self.multi else self.dir
        return fill(self.data.get("out", default), self.values(arch, default))

    def values(self, arch: str, out: str | None = None) -> dict[str, str]:
        values = {"dir": self.dir, "arch": arch, "platform": PLATFORMS[arch], "name": self.name}
        values["out"] = out if out is not None else self.out(arch)
        return values

    @property
    def published(self) -> list[tuple[str, str]]:
        """(directory, architecture or '') of each archive this product publishes."""
        if self.index:
            return [(f"{self.dir}/index", "")]
        return [(self.out(arch), arch if self.multi else "") for arch in self.arches]

    @property
    def last(self) -> list[str]:
        """The checks whose archives are published."""
        return [f"image-{self.name}"] if self.index else [self.build_name(a) for a in self.arches]

    def requires(self, arch: str, fields: dict[str, Any]) -> list[str]:
        extra = [r for r in fields.pop("requires", []) if not r.startswith("arch=")]
        return [f"arch={arch}", *extra]

    def steps(self) -> list[str]:
        out = []
        about = self.data.get("about")
        context = self.data.get("context")
        context_name = None
        if context:
            context_name = context.get("name", f"image-{self.name}-context")
            fields = fill({k: v for k, v in context.items() if k not in STEP_KEYS}, self.values(self.arches[0]))
            self.own(fields, "context")
            fields = {
                "name": context_name,
                "run": fill(context["run"], self.values(self.arches[0])),
                **fields,
                "requires": self.requires(self.arches[0], dict(fields)),
                "default": fields.get("default", False),
            }
            fields["inputs"] = dedupe([f"{PUBLISH}/lib.sh", *fields.get("inputs", [])])
            out.append(check(fields, about))
            about = None
        for arch in self.arches:
            out.append(check(self.build(arch, context_name), about))
            about = None
        if self.index:
            dirs = [self.out(arch) for arch in self.arches]
            out.append(check({
                "name": f"image-{self.name}",
                "run": f"python3 {PUBLISH}/oci-index.py {self.dir}/index "
                       + " ".join(f"{d}/image.oci.tar" for d in dirs),
                "inputs": [f"{PUBLISH}/oci-index.py"],
                "needs": [self.build_name(a) for a in self.arches],
                "outputs": [f"{self.dir}/index/image.oci.tar", f"{self.dir}/index/digest"],
                "requires": ["arch=any"],
                "default": False,
            }))
        # A product with no tags is built and tested only (its publication isn't on t yet).
        if self.release:
            out.append(check(self.sign()))
        if self.dev or self.release:
            out.append(check(self.publish()))
        return out

    def own(self, fields: dict[str, Any], where: str) -> None:
        clash = OWN_FIELDS & (set(fields) - {"needs"})
        if clash:
            raise ProductError(f"{self.name}: {where} may not set {sorted(clash)}")

    def build(self, arch: str, context_name: str | None) -> dict[str, Any]:
        spec = self.data["build"]
        values = self.values(arch)
        fields = fill({k: v for k, v in spec.items() if k not in STEP_KEYS}, values)
        self.own(fields, "build")
        lines = []
        if "run" in spec:
            lines.append(fill(spec["run"], values))
        else:
            # Without a context the build sees an empty directory: a Containerfile that copies
            # nothing from the tree reads only itself.
            ctx = fill(spec.get("context", ""), values)
            if not ctx:
                lines.append("mkdir -p $TMPDIR/empty")
            args = [ctx or "$TMPDIR/empty", fill(spec["containerfile"], values), values["out"]]
            args += fill(spec.get("build_args", []), values)
            lines.append(f"bash {PUBLISH}/oci-build.sh " + " ".join(args))
        lines += [fill(line, values) for line in spec.get("test", [])]
        run = lines[0] if len(lines) == 1 else "\n".join(lines) + "\n"
        needs = [*([context_name] if context_name else []), *fields.pop("needs", [])]
        result = {
            "name": self.build_name(arch),
            "image": fields.pop("image", self.defaults["buildah"]),
            "run": run,
            **fields,
            "inputs": dedupe([*BUILD_INPUTS, *fields.get("inputs", [])]),
            "outputs": [f"{values['out']}/image.oci.tar", f"{values['out']}/digest", *fields.get("outputs", [])],
            "requires": self.requires(arch, dict(fields)),
            "default": fields.get("default", False),
        }
        if needs:
            result["needs"] = needs
        if not result["image"]:
            del result["image"]  # `image = ""`: t.toml's default image, the CI builder
        elif result["image"] == self.defaults["buildah"] and not result.get("privileged"):
            # buildah needs user namespaces and mounts the job sandbox's strict tier refuses.
            result["isolation"] = "vm"
        return result

    def sign(self) -> dict[str, Any]:
        env = dict(self.defaults["signing_env"])
        if self.data.get("tlog"):
            env["TILO_SIGN_TLOG"] = "1"
        outputs = []
        for d, _ in self.published:
            # dry-run.pub exists only in a dry run (TILO_PUBLISH_DRY_RUN): the throwaway key's
            # public half, which the publishing check's dry run verifies the signature against.
            outputs += [f"{d}/payload.json", f"{d}/signature", f"{d}/dry-run.pub"]
            if self.data.get("tlog"):
                outputs.append(f"{d}/bundle.json")
        return {
            "name": f"sign-{self.name}",
            "image": self.defaults["skopeo"],
            "run": "\n".join(f"bash {PUBLISH}/oci-sign.sh {self.repository} {d}" for d, _ in self.published)
                   + ("\n" if len(self.published) > 1 else ""),
            "inputs": SIGN_INPUTS,
            "needs": [*self.last, *self.gates],
            "outputs": outputs,
            "id_token": ["sts.amazonaws.com"],
            "env": env,
            "requires": CREDENTIAL_REQUIRES,
            "on": self.release_on,
        }

    def publish(self) -> dict[str, Any]:
        spec = fill(self.data.get("publish", {}), self.values(self.arches[0]))
        if set(spec) - {"run", "inputs", "outputs", "pointers"}:
            raise ProductError(f"{self.name}: publish takes run, inputs, outputs and pointers")
        if self.dev and self.release:
            channel = "any"
            on = [*self.dev_on, *self.release_on]
        elif self.dev:
            channel, on = "dev", self.dev_on
        else:
            channel, on = "release", self.release_on
        lines, outputs = [], []
        for d, arch in self.published:
            if channel == "any":
                tags = [f"dev:{t}" for t in self.dev] + [f"release:{t}" for t in self.release]
            else:
                tags = self.dev or self.release
            tags = [t.replace("{arch}", arch) for t in tags]
            if "run" not in spec:
                lines.append(f"bash {PUBLISH}/oci-publish.sh {channel} {self.repository} {d} "
                             + " ".join(f"'{t}'" for t in tags))
            outputs.append(f"{d}/receipt.json")
            if self.data.get("anonymous"):
                lines.append(f"bash {PUBLISH}/oci-anonymous.sh {self.repository} {d}")
        # A publication of its own (tags the build decides) still gets the chain's needs and
        # triggers; it calls oci-publish.sh itself.
        if "run" in spec:
            lines.insert(0, spec["run"])
        if self.data.get("after_publish"):
            lines.append(fill(self.data["after_publish"], self.values(self.arches[0])))
        inputs = [*PUBLISH_INPUTS, *([f"{PUBLISH}/oci-anonymous.sh"] if self.data.get("anonymous") else [])]
        fields = {
            "name": f"publish-{self.name}",
            "image": self.defaults["skopeo"],
            "run": lines[0] if len(lines) == 1 else "\n".join(lines) + "\n",
            "inputs": dedupe([*inputs, *spec.get("inputs", [])]),
            "needs": [*self.last, *self.gates, *([f"sign-{self.name}"] if channel == "release" else [])],
            "outputs": dedupe([*outputs, *spec.get("outputs", [])]),
            "id_token": ["registry.tilo.so"],
            "requires": CREDENTIAL_REQUIRES,
            "on": on,
        }
        if channel == "any":
            fields["optional_needs"] = [f"sign-{self.name}"]
        if "pointers" in spec:
            fields["pointers"] = spec["pointers"]
        return fields


def placeholders(v: Any, values: dict[str, str]) -> Any:
    """Replace {name} for each of VALUES in strings, leaving other braces ({commit}) alone."""
    if isinstance(v, str):
        return re.sub(r"\{(\w+)\}", lambda m: values.get(m[1], m[0]), v)
    if isinstance(v, list):
        return [placeholders(x, values) for x in v]
    if isinstance(v, dict):
        return {k: placeholders(x, values) for k, x in v.items()}
    return v


def matrix(data: dict[str, Any]) -> list[str]:
    """Checks that differ only by architecture, written once with placeholders.

    `each` lists the variants, each a table of placeholder values (`arch`, and whatever else the
    checks name, such as `deb = "amd64"`). Every `[[matrix.check]]` is written for each variant,
    in order; one with `only` is written for those variants whose `arch` it lists, and one whose
    name has no placeholder is written once, after the variants. `fields` go to every check
    that doesn't set them itself.
    """
    unknown = set(data) - {"family", "about", "each", "check", "fields"}
    if unknown:
        raise ProductError(f"matrix {data.get('about', '')[:40]!r}: unknown keys {sorted(unknown)}")
    out, about = [], data.get("about")
    once = [c for c in data["check"] if "{" not in c["name"]]
    for values in data["each"]:
        for spec in data["check"]:
            if spec in once or values["arch"] not in spec.get("only", [values["arch"]]):
                continue
            fields = placeholders({**data.get("fields", {}),
                                   **{k: v for k, v in spec.items() if k not in ("only", "comment")}}, values)
            comment = "\n".join(x for x in (about, placeholders(spec.get("comment", ""), values)) if x)
            out.append(check(fields, comment or None))
            about = None
    for spec in once:
        out.append(check({**data.get("fields", {}), **{k: v for k, v in spec.items() if k != "comment"}},
                         spec.get("comment")))
    return out


def render(manifest: dict[str, Any], family: str) -> str:
    """The checks of the products and matrices of one family, in manifest order."""
    defaults = manifest["defaults"]
    blocks = []
    for data in manifest.get("product", []):
        if data.get("family", "") == family:
            data = {k: v for k, v in data.items() if k != "family"}
            blocks.append("\n".join(Product(data, defaults).steps()))
    for data in manifest.get("matrix", []):
        if data.get("family", "") == family:
            blocks.append("\n".join(matrix(data)))
    return "\n".join(blocks)


def families(manifest: dict[str, Any]) -> list[str]:
    return dedupe([p["family"] for p in [*manifest.get("product", []), *manifest.get("matrix", [])]])

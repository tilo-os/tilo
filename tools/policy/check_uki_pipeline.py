#!/usr/bin/env python3
"""Keep the sealing stages of every sealed image one pipeline.

Three Containerfiles seal an image: os/Containerfile (production),
os/Containerfile.board and os/Containerfile.dev-installer-base. Each splits the
kernel out of its rootfs twice, in a throwaway `kernel` stage the UKI embeds and
in the shipped `split` rootfs, installs ukify in a throwaway `uki-tools` stage,
runs os/dev/uki.sh in a `uki` stage and adds the UKI under /boot in `final`. They
used to be hand-copied, and the copies drifted: a different package list, seal
inputs one image declared and the others silently defaulted. A change to the
sealing contract now has to reach all three, so this check fails when:

  * a `uki-tools` stage's instructions differ from os/Containerfile's (only its
    FROM line may), or it names packages instead of installing
    os/dev/uki-tools.packages, the one list of sealing tools;
  * a `uki` stage does not declare the seal inputs (TILO_CFS_DIGEST,
    TILO_SEAL_GENERATION) exactly as os/Containerfile does, or does anything
    but run os/dev/uki.sh over the split rootfs and the `kernel` stage's kernel;
  * a `kernel` stage does not end in bootc's kernel split of the same root
    `split` builds on;
  * a `split` stage does not end in bootc's kernel split, optionally followed by
    dropping the split-out initramfs the UKI carries (os/Containerfile, #4055),
    or a `final` stage adds anything to the sealed rootfs beyond the UKI under
    /boot;
  * os/dev/uki-tools.packages is not in the frozen RPM snapshot's package set,
    or a hand-assembled build context (the board's .dockerignore, the dev
    installer workflow's adapter context) lacks a file the stages COPY.

The composefs probe (os/dev/composefs/) is a deliberately independent copy; its
header says why.

Run: python3 tools/policy/check_uki_pipeline.py
"""

from __future__ import annotations

import importlib.util
import re
import shlex
import sys
import tempfile
from dataclasses import dataclass
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
RERUN = "python3 tools/policy/check_uki_pipeline.py   (or tools/policy/run.sh)"
REFERENCE = "os/Containerfile"
IMAGES = (REFERENCE, "os/Containerfile.board", "os/Containerfile.dev-installer-base")
PACKAGES = "os/dev/uki-tools.packages"
DERIVE = "os/dev/derive-rpm-package-set.py"
BOARD_IGNORE = "os/Containerfile.board.dockerignore"
ADAPTER_WORKFLOW = ".forgejo/workflows/os-dev-installer.yml"
ADAPTER_IMAGE = "os/Containerfile.dev-installer-base"
ADAPTER_COPY = re.compile(r'\bcp\s+((?:[^\n"]|\\\n)*?)\s*"\$adapter_context/os/dev/"')

SEAL_INPUTS = (
    "ARG TILO_CFS_DIGEST=",
    "ENV CFS_DIGEST=${TILO_CFS_DIGEST}",
    "ARG TILO_SEAL_GENERATION=1",
    "ENV TILO_SEAL_GENERATION=${TILO_SEAL_GENERATION}",
)
UKI_MOUNTS = (
    "--mount=type=tmpfs,target=/tmp",
    "--mount=type=tmpfs,target=/run",
    "--mount=type=bind,from=kernel,source=/kernel,target=/kernel,rw",
)
UKI_COMMAND = "bash /usr/local/bin/uki.sh"
SPLIT_COMMAND = "bootc container split-kernel-and-rootfs --rootfs / --output /kernel"
# What `split` may do after the split: drop the initramfs the UKI's .initrd
# section already carries, so the image does not ship it twice.
SPLIT_DISCARD = "rm /kernel/*/initramfs.img"
FINAL_UKI = "COPY --from=uki /out/*.efi /boot/EFI/Linux/"
# What `final` may carry besides the UKI: metadata, never a rootfs change.
FINAL_METADATA = ("ARG", "LABEL")


@dataclass(frozen=True)
class Finding:
    where: str
    what: str
    why: str
    fix: str

    def __str__(self) -> str:
        return f"{self.where}: {self.what}\n  why: {self.why}.\n  fix: {self.fix}\n  re-run: {RERUN}"


@dataclass(frozen=True)
class Instruction:
    line: int
    text: str

    @property
    def keyword(self) -> str:
        return self.text.split(" ", 1)[0].upper()


@dataclass(frozen=True)
class Stage:
    line: int
    base: str
    body: tuple[Instruction, ...]


def instructions(text: str) -> list[Instruction]:
    """Instructions with comments dropped, continuations joined, whitespace collapsed."""
    found: list[Instruction] = []
    pending: list[str] = []
    start = 0
    for number, physical in enumerate(text.splitlines(), 1):
        stripped = physical.strip()
        if stripped.startswith("#") or (not stripped and not pending):
            continue
        if not pending:
            start = number
        if stripped.endswith("\\"):
            pending.append(stripped[:-1])
            continue
        pending.append(stripped)
        found.append(Instruction(start, " ".join(" ".join(pending).split())))
        pending = []
    if pending:
        found.append(Instruction(start, " ".join(" ".join(pending).split())))
    return found


def stages(text: str) -> dict[str, Stage]:
    found: dict[str, Stage] = {}
    name, line, base, body = None, 0, "", []
    for instruction in instructions(text):
        words = instruction.text.split()
        if instruction.keyword == "FROM":
            if name:
                found[name] = Stage(line, base, tuple(body))
            name = words[3] if len(words) >= 4 and words[2].upper() == "AS" else None
            line, base, body = instruction.line, words[1] if len(words) > 1 else "", []
        elif name:
            body.append(instruction)
    if name:
        found[name] = Stage(line, base, tuple(body))
    return found


def load_derive(root: Path):
    spec = importlib.util.spec_from_file_location("uki_pipeline_derive", root / DERIVE)
    module = importlib.util.module_from_spec(spec)
    assert spec.loader is not None
    spec.loader.exec_module(module)
    return module


def copy_sources(stage: Stage) -> list[str]:
    """The build-context sources of a stage's COPY instructions."""
    sources: list[str] = []
    for instruction in stage.body:
        if instruction.keyword != "COPY":
            continue
        words = shlex.split(instruction.text)[1:]
        if any(word.startswith("--from=") for word in words):
            continue
        sources.extend(word for word in words[:-1] if not word.startswith("--"))
    return sources


def last_command(run: Instruction) -> str:
    """The shell command a RUN ends with, past its `--mount` flags and any `&&` chain."""
    words = run.text.split()[1:]
    while words and words[0].startswith("--"):
        words.pop(0)
    return " ".join(words).rsplit("&&", 1)[-1].strip()


def audit_uki_tools(name: str, stage: Stage, reference: Stage, derive) -> list[Finding]:
    where = f"{name}:{stage.line}"
    findings: list[Finding] = []
    if [i.text for i in stage.body] != [i.text for i in reference.body]:
        theirs, ours = [i.text for i in stage.body], [i.text for i in reference.body]
        index = next((n for n, (a, b) in enumerate(zip(theirs, ours)) if a != b), min(len(theirs), len(ours)))
        line = stage.body[index].line if index < len(stage.body) else stage.line
        findings.append(Finding(
            f"{name}:{line}", f"the `uki-tools` stage differs from {REFERENCE}'s "
            f"(first difference: {theirs[index] if index < len(theirs) else '<missing>'!r}, "
            f"expected {ours[index] if index < len(ours) else '<nothing>'!r})",
            "every sealed image must seal with the same tools; a tool or step added to one copy fails or "
            "silently changes the others' UKI only when that image is built",
            f"make this stage's instructions after FROM identical to {REFERENCE}'s `uki-tools` stage"))
    if name == REFERENCE:
        if not any(i.keyword == "COPY" and f" {PACKAGES} " in f" {i.text} " for i in stage.body):
            findings.append(Finding(
                where, f"the `uki-tools` stage does not install {PACKAGES}",
                "that file is the one declaration of the sealing tools, and the RPM snapshot is derived from it",
                f"COPY {PACKAGES} into the stage and install it with `grep -v '^#' ... | xargs dnf install`"))
        inline = inline_packages(derive, stage)
        if inline:
            findings.append(Finding(
                where, f"the `uki-tools` stage names packages inline: {', '.join(inline)}",
                f"the sealing tools are declared once, in {PACKAGES}; a second list drifts",
                f"add them to {PACKAGES} instead"))
    return findings


def inline_packages(derive, stage: Stage) -> list[str]:
    """Packages a stage names in a dnf install, read the way the snapshot reads them."""
    with tempfile.NamedTemporaryFile("w", suffix=".Containerfile") as handle:
        handle.write("\n".join(i.text for i in stage.body))
        handle.flush()
        return sorted(set(derive.packages(Path(handle.name))))


def audit_uki(name: str, stage: Stage) -> list[Finding]:
    where = f"{name}:{stage.line}"
    findings: list[Finding] = []
    if stage.base != "uki-tools":
        findings.append(Finding(where, f"the `uki` stage builds FROM {stage.base}, not uki-tools",
                                "the seal must be made by the shared tool stage", "use `FROM uki-tools AS uki`"))
    declared = tuple(i.text for i in stage.body if i.keyword in ("ARG", "ENV"))
    if declared != SEAL_INPUTS:
        findings.append(Finding(
            where, f"the `uki` stage declares {list(declared)}, not the seal inputs {list(SEAL_INPUTS)}",
            "os/dev/uki.sh reads its inputs from the environment and defaults what is unset, so an image that "
            "lacks one builds a UKI under a different contract and nothing fails",
            "declare exactly these ARG/ENV lines, in this order, as os/Containerfile does"))
    runs = [i for i in stage.body if i.keyword == "RUN"]
    others = [i for i in stage.body if i.keyword not in ("ARG", "ENV", "RUN")]
    for instruction in others:
        findings.append(Finding(f"{name}:{instruction.line}", f"the `uki` stage carries `{instruction.text}`",
                                "the stage only runs os/dev/uki.sh; anything else is a fork of the pipeline",
                                "move it into os/dev/uki.sh or the shared uki-tools stage"))
    if len(runs) != 1:
        findings.append(Finding(where, f"the `uki` stage has {len(runs)} RUN instructions, not one",
                                "the seal is the one os/dev/uki.sh run", f"run `{UKI_COMMAND}` once"))
        return findings
    run = runs[0]
    missing = [mount for mount in UKI_MOUNTS if mount not in run.text.split()]
    if missing or "target=/target" not in run.text or last_command(run) != UKI_COMMAND:
        findings.append(Finding(
            f"{name}:{run.line}", f"the `uki` RUN is not `{UKI_COMMAND}` over the split rootfs at /target and its "
            f"kernel at /kernel{' (missing ' + ', '.join(missing) + ')' if missing else ''}",
            "os/dev/uki.sh seals what is mounted at /target and /kernel, with build scratch on tmpfs",
            f"mount {', '.join(UKI_MOUNTS)} and the rootfs at /target, and end the RUN with `{UKI_COMMAND}`"))
    return findings


def ends_with_split(stage: Stage, discard: bool) -> bool:
    """The stage's last instruction is a RUN whose last command is the split,
    or (with `discard`) the split followed by dropping the initramfs."""
    if not stage.body or stage.body[-1].keyword != "RUN":
        return False
    commands = [command.strip() for command in last_command_chain(stage.body[-1])]
    if commands[-1] == SPLIT_COMMAND:
        return True
    return discard and len(commands) >= 2 and commands[-2:] == [SPLIT_COMMAND, SPLIT_DISCARD]


def last_command_chain(run: Instruction) -> list[str]:
    """The `&&`-separated commands of a RUN, past its `--mount` flags."""
    words = run.text.split()[1:]
    while words and words[0].startswith("--"):
        words.pop(0)
    return " ".join(words).split("&&")


def audit_layout(name: str, found: dict[str, Stage]) -> list[Finding]:
    findings: list[Finding] = []
    kernel, split, final = found.get("kernel"), found.get("split"), found.get("final")
    if kernel is not None:
        runs = [i for i in kernel.body if i.keyword == "RUN"]
        if not ends_with_split(kernel, discard=False):
            line = runs[-1].line if runs else kernel.line
            findings.append(Finding(
                f"{name}:{line}", f"the `kernel` stage does not end with `{SPLIT_COMMAND}`",
                "the UKI embeds the kernel and initramfs this stage splits out of the rootfs",
                f"make `{SPLIT_COMMAND}` the stage's last command"))
        if split is not None and kernel.base != split.base:
            findings.append(Finding(
                f"{name}:{kernel.line}", f"`kernel` builds FROM {kernel.base}, not {split.base} as `split` does",
                "the UKI must embed the kernel of the very rootfs it seals",
                f"use `FROM {split.base} AS kernel`"))
    if split is not None:
        runs = [i for i in split.body if i.keyword == "RUN"]
        if not ends_with_split(split, discard=True):
            line = runs[-1].line if runs else split.line
            findings.append(Finding(
                f"{name}:{line}", f"the `split` stage does not end with `{SPLIT_COMMAND}`",
                "the sealed rootfs is exactly what the split leaves; a later step changes the digest the UKI seals",
                "run the image's own assertions first and make the split the stage's last command "
                f"(optionally followed by `{SPLIT_DISCARD}`)"))
    if final is not None:
        where = f"{name}:{final.line}"
        if final.base != "split":
            findings.append(Finding(where, f"`final` builds FROM {final.base}, not split",
                                    "the UKI seals the split rootfs, so final must be that rootfs",
                                    "use `FROM split AS final`"))
        if not final.body or final.body[0].text != FINAL_UKI:
            findings.append(Finding(where, f"`final` does not start with `{FINAL_UKI}`",
                                    "the UKI under /boot is the one thing final adds",
                                    f"make `{FINAL_UKI}` its first instruction"))
        for instruction in final.body[1:]:
            if instruction.keyword not in FINAL_METADATA:
                findings.append(Finding(
                    f"{name}:{instruction.line}", f"`final` carries `{instruction.text}`",
                    "anything added to the rootfs after the split changes the composefs digest the UKI seals, "
                    "and `bootc install` refuses the image",
                    "move it before the split stage; final may add only the UKI and metadata"))
    return findings


def audit_contexts(root: Path, sources: list[str]) -> list[Finding]:
    findings: list[Finding] = []
    ignore = root / BOARD_IGNORE
    allowed = {line.strip()[1:].rstrip("/") for line in ignore.read_text(encoding="utf-8").splitlines()
               if line.strip().startswith("!")} if ignore.exists() else set()
    for source in sources:
        if source not in allowed:
            findings.append(Finding(
                BOARD_IGNORE, f"the board's sparse context does not include `{source}`",
                "os/Containerfile.board's uki-tools stage copies it, and BuildKit sends only what this file allows",
                f"add `!{source}`"))
    workflow = root / ADAPTER_WORKFLOW
    match = ADAPTER_COPY.search(workflow.read_text(encoding="utf-8")) if workflow.exists() else None
    copied = set(shlex.split(match.group(1).replace("\\\n", " "))) if match else set()
    if not match:
        findings.append(Finding(
            ADAPTER_WORKFLOW, "no `cp ... \"$adapter_context/os/dev/\"` assembling the dev installer's context",
            f"{ADAPTER_IMAGE} builds from that hand-assembled context, so this check cannot see what it holds",
            "keep the adapter context a single cp into \"$adapter_context/os/dev/\", or update this check"))
    for source in sources if match else []:
        if source not in copied:
            findings.append(Finding(
                ADAPTER_WORKFLOW, f"the dev installer's adapter context does not copy `{source}`",
                f"{ADAPTER_IMAGE}'s uki-tools stage copies it from that context, and the build fails without it",
                f"add {source} to the cp into \"$adapter_context/os/dev/\""))
    return findings


def audit(root: Path = REPO) -> list[Finding]:
    findings: list[Finding] = []
    derive = load_derive(root)
    if PACKAGES not in derive.SOURCES:
        findings.append(Finding(DERIVE, f"{PACKAGES} is not a snapshot package source",
                                "the uki-tools stages install it from the frozen RPM snapshot, offline",
                                f"add {PACKAGES} to SOURCES"))
    if not (root / PACKAGES).exists() or not list(derive.packages(root / PACKAGES)):
        findings.append(Finding(PACKAGES, "the sealing tool list is missing or empty",
                                "every uki-tools stage installs it", "restore systemd-ukify and friends"))
    parsed = {name: stages((root / name).read_text(encoding="utf-8")) for name in IMAGES}
    reference = parsed[REFERENCE].get("uki-tools")
    for name, found in parsed.items():
        missing = [stage for stage in ("kernel", "split", "uki-tools", "uki", "final") if stage not in found]
        if missing:
            findings.append(Finding(name, f"no {', '.join(missing)} stage",
                                    "every sealed image carries the shared sealing stages",
                                    f"name the stages as {REFERENCE} does"))
        if reference is not None and "uki-tools" in found:
            findings += audit_uki_tools(name, found["uki-tools"], reference, derive)
        if "uki" in found:
            findings += audit_uki(name, found["uki"])
        findings += audit_layout(name, found)
    if reference is not None:
        findings += audit_contexts(root, copy_sources(reference))
    return findings


def main() -> int:
    findings = audit()
    for finding in findings:
        # The finding on stdout, its why, fix and re-run under it on stderr (findings.py).
        headline, detail = str(finding).split("\n", 1)
        print(headline)
        print(detail, file=sys.stderr)
    if findings:
        print(f"\n{len(findings)} finding(s) in the shared UKI sealing pipeline.", file=sys.stderr)
        return 1
    print("Sealed images share one UKI pipeline.", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())

"""Read canonical activation receipts without replacing their provenance."""
import re
from pathlib import PurePosixPath

from .action_jobs import require

COMPONENTS = ("launcher", "install", "apps+runtime", "app-host", "native-runtime-host",
              "control", "app-cli", "ush", "shell")
SOURCE_FIELDS = ("source_commit", "dirty_tree_hash", "cargo_lock_hash")


def hex_value(value, length):
    return isinstance(value, str) and re.fullmatch(r"[0-9a-f]{" + str(length) + r"}", value) is not None


def selected_layer(receipt, shown, vm, slot):
    require(isinstance(receipt, dict) and isinstance(shown, dict), "canonical cohort metadata missing")
    source = receipt.get("cohort_source")
    require(receipt.get("tool") == "tilo-vm" and receipt.get("status") == "success"
            and receipt.get("vm") == vm and receipt.get("slot") == slot
            and receipt.get("component") in ("apps+runtime", "userspace"),
            "successful addressed canonical cohort activation receipt required")
    require(isinstance(source, dict) and hex_value(source.get("source_commit"), 40)
            and hex_value(source.get("dirty_tree_hash"), 64), "full source provenance required")
    current = shown.get("slot")
    require(isinstance(current, dict) and current.get("name") == slot
            and current.get("session") == "active" and isinstance(current.get("components"), dict),
            "addressed active cohort slot unobservable")
    if receipt["component"] == "apps+runtime":
        layer = receipt.get("layer")
        require(hex_value(layer, 64) and current["components"].get("apps+runtime") == layer,
                "cohort inactive or changed")
        return layer

    activation = receipt.get("cohort_activation")
    require(isinstance(activation, dict) and activation.get("failure", "missing") is None
            and activation.get("source") == source
            and hex_value(receipt.get("source_inventory"), 64)
            and activation.get("source_inventory") == receipt["source_inventory"],
            "complete source-bound userspace activation required")
    result = receipt.get("result")
    require(isinstance(result, dict) and all(result.get(key) == activation.get(key)
            for key in ("apply", "source", "source_inventory", "members", "phases", "failure")),
            "successful userspace result differs from activation")
    members = receipt.get("cohort_members")
    require(isinstance(members, list) and len(members) == len(COMPONENTS)
            and members == activation.get("members"), "complete canonical userspace members required")
    require(source.get("component") == "userspace" and source.get("authority") == "local"
            and source.get("builder") == "remote" and hex_value(source.get("cargo_lock_hash"), 64),
            "canonical remote worktree source required")
    targets, toolchains = set(), set()
    for member, component in zip(members, COMPONENTS):
        require(isinstance(member, dict) and member.get("component") == component
                and hex_value(member.get("layer"), 64)
                and member.get("source_inventory") == receipt["source_inventory"],
                "userspace member order, layer or inventory differs")
        build = member.get("build")
        require(isinstance(build, dict) and build.get("component") == component
                and all(build.get(key) == source[key] for key in SOURCE_FIELDS)
                and build.get("authority") == "local" and build.get("builder") == "remote"
                and build.get("target") in ("aarch64-unknown-linux-gnu", "x86_64-unknown-linux-gnu")
                and isinstance(build.get("toolchain"), str) and bool(build["toolchain"])
                and build.get("cargo_profile") == ("thin-app" if component == "apps+runtime" else "release"),
                "userspace member build provenance differs")
        require(current["components"].get(component) == member["layer"],
                "selected userspace component differs from successful activation")
        targets.add(build["target"])
        toolchains.add(build["toolchain"])
    require(len(targets) == 1 and len(toolchains) == 1, "userspace build architecture or toolchain differs")
    apply = activation.get("apply")
    require(isinstance(apply, dict) and apply.get("slot") == slot
            and type(apply.get("generation")) is int and apply["generation"] > 0
            and type(current.get("generation")) is int and current["generation"] == apply["generation"]
            and apply.get("policy") == "session_restart" and apply.get("boot") is None,
            "userspace generation or session activation differs")
    identity = current.get("session_identity")
    require(isinstance(identity, dict) and isinstance(identity.get("user"), str)
            and bool(identity["user"]) and current.get("user") == identity["user"]
            and isinstance(identity.get("unit"), str) and bool(identity["unit"]),
            "current owner session identity unobservable")
    actions = apply.get("actions")
    require(isinstance(actions, list) and all(isinstance(action, dict)
            and action.get("action") != "deferred"
            and ("user" not in action or action["user"] == identity["user"]) for action in actions),
            "userspace activation deferred or targets a different owner")
    if identity.get("login_session") is not None:
        require({"action": "restart_desktop", "user": identity["user"]} in actions,
                "owner desktop activation unobservable")
    else:
        require({"action": "start_unit", "unit": identity["unit"]} in actions,
                "owned session activation unobservable")
    return members[COMPONENTS.index("apps+runtime")]["layer"]


def runtime_inventory(shown, layer):
    rows = shown.get("files")
    require(isinstance(rows, list), "selected SDK file inventory unobservable")
    installed = {}
    for row in rows:
        require(isinstance(row, dict), "malformed selected file inventory")
        if row.get("layer") != layer or row.get("component") != "apps+runtime":
            continue
        path = row.get("path")
        require(isinstance(path, str), "selected SDK path unobservable")
        if not path.startswith("/usr/lib/tilo-runtime/"):
            continue
        require(str(PurePosixPath(path)) == path and ".." not in PurePosixPath(path).parts,
                "selected SDK path escapes runtime")
        if "link" in row:
            require("sha256" not in row, "ambiguous SDK link inventory")
            continue
        require(hex_value(row.get("sha256"), 64), "selected SDK content hash unobservable")
        mapped = path.replace("/usr/lib/tilo-runtime/", "/run/tilo-runtime/", 1)
        require(mapped not in installed, "ambiguous selected SDK file inventory")
        installed[mapped] = row["sha256"]
    require(sum(path.endswith("/lib/libruntime_dylib.so") for path in installed) == 1,
            "closed SDK unobservable or ambiguous")
    return installed

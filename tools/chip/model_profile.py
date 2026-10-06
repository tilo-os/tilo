#!/usr/bin/env python3
"""Pin model metadata and calculate explicitly limited storage bounds.

Downloads config/metadata only, never weights or executable model code. Reports
are inputs to the Tilo engineering workbench, not ASIC area/power predictions.
"""

import argparse
import hashlib
import json
from pathlib import Path
import re
import urllib.request


def read_url(url):
    with urllib.request.urlopen(url, timeout=30) as response:
        data = response.read(4_000_001)
    if len(data) > 4_000_000:
        raise ValueError("metadata exceeds 4 MB limit")
    return json.loads(data)


def snapshot(model):
    if not re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", model):
        raise ValueError("model must be an owner/name identifier")
    metadata = read_url(f"https://huggingface.co/api/models/{model}")
    revision = metadata["sha"]
    if not re.fullmatch(r"[0-9a-f]{40}", revision):
        raise ValueError("model revision must be an immutable commit")
    url = f"https://huggingface.co/{model}/resolve/{revision}/config.json"
    config = read_url(url)
    return {
        "schema_version": 1,
        "model": model,
        "revision": revision,
        "config_url": url,
        "license_at_capture": metadata.get("cardData", {}).get("license"),
        "tensor_inventory_at_capture": metadata.get("safetensors"),
        "config_sha256": config_digest(config),
        "config": config,
    }


def config_digest(config):
    canonical = json.dumps(config, sort_keys=True, separators=(",", ":"))
    return hashlib.sha256(canonical.encode()).hexdigest()


def positive_integer(value, name):
    if isinstance(value, bool) or not isinstance(value, int) or value <= 0:
        raise ValueError(f"{name} must be a positive integer")
    return value


def attention_cache(config, context, bits):
    """Conventional GQA cache only; reject specialized cache architectures."""
    kind = config.get("model_type", "")
    supported = {"qwen3_5_text", "qwen3_5_moe_text", "qwen3_next", "ministral3"}
    if kind not in supported:
        return {"status": "unsupported", "reason": f"cache formula not verified for {kind}"}
    layers = positive_integer(config["num_hidden_layers"], "num_hidden_layers")
    types = config.get("layer_types")
    if types is not None:
        if len(types) != layers or set(types) - {"linear_attention", "full_attention"}:
            raise ValueError("invalid layer_types")
        full = types.count("full_attention")
    elif kind in {"qwen3_5_text", "qwen3_5_moe_text"}:
        raise ValueError("hybrid model requires explicit layer_types")
    elif kind == "qwen3_next":
        interval = positive_integer(config["full_attention_interval"], "full_attention_interval")
        full = layers // interval
    else:
        full = layers
    heads = positive_integer(config["num_key_value_heads"], "num_key_value_heads")
    dimension = positive_integer(config["head_dim"], "head_dim")
    elements = 2 * full * heads * dimension * context
    return {
        "status": "partial_bound",
        "full_attention_layers": full,
        "kv_elements": elements,
        "packed_bytes": (elements * bits + 7) // 8,
        "excludes": ["recurrent_state", "convolution_state", "scales", "padding", "workspace", "batching"],
    }


def profile(record, assumptions):
    if record.get("schema_version") != 1:
        raise ValueError("unsupported snapshot schema")
    if not re.fullmatch(r"[0-9a-f]{40}", record.get("revision", "")):
        raise ValueError("snapshot lacks an immutable revision")
    model = record.get("model", "")
    if not re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", model):
        raise ValueError("invalid model identifier")
    expected_url = f"https://huggingface.co/{model}/resolve/{record['revision']}/config.json"
    if record.get("config_url") != expected_url:
        raise ValueError("config URL does not match model and revision")
    config = record["config"]
    if config_digest(config) != record.get("config_sha256"):
        raise ValueError("config digest mismatch")
    for name, value in assumptions.items():
        positive_integer(value, name)
    context = assumptions["context_tokens"]
    text = config.get("text_config", config)
    if context > positive_integer(text["max_position_embeddings"], "max_position_embeddings"):
        raise ValueError("context exceeds pinned config limit")
    weight_bits = assumptions["weight_bits"]
    kv_bits = assumptions["kv_bits"]
    if weight_bits > 32 or kv_bits > 32:
        raise ValueError("precision must be between 1 and 32 bits")
    # Parameter count is supplied explicitly. Quantized tensor inventories can
    # contain packed weights and are not a reliable logical parameter count.
    weights = (assumptions["logical_parameters"] * weight_bits + 7) // 8
    return {
        "schema_version": 1,
        "model": record["model"],
        "revision": record["revision"],
        "config_sha256": record["config_sha256"],
        "evidence": "analytical_storage_bound",
        "batch_size": 1,
        "assumptions": assumptions,
        "packed_weight_bytes": weights,
        "full_attention_cache": attention_cache(text, context, kv_bits),
        "not_estimated": ["silicon_area", "power", "tokens_per_second", "coding_quality", "total_runtime_memory"],
        "limitations": [
            "Weight bound assumes uniform precision; excludes scales, padding and duplicated storage.",
            "Logical parameter count is a user assumption, not inferred from packed tensor inventory.",
            "Open-weight access does not by itself establish hardware commercialization rights.",
            "Snapshot digest checks integrity, not publisher authentication.",
            "Precision is a storage assumption; quantization quality has not been validated.",
        ],
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    pin = commands.add_parser("snapshot", help="download only metadata at an immutable revision")
    pin.add_argument("model")
    pin.add_argument("--out", type=Path, required=True)
    report = commands.add_parser("profile", help="calculate storage bounds from a saved snapshot")
    report.add_argument("snapshot", type=Path)
    report.add_argument("--logical-parameters", type=int, required=True)
    report.add_argument("--weight-bits", type=int, default=4)
    report.add_argument("--kv-bits", type=int, default=16)
    report.add_argument("--context-tokens", type=int, default=32768)
    args = parser.parse_args()
    try:
        if args.command == "snapshot":
            result = snapshot(args.model)
            args.out.parent.mkdir(parents=True, exist_ok=True)
            with args.out.open("x") as output:
                json.dump(result, output, indent=2, sort_keys=True)
                output.write("\n")
        else:
            result = profile(json.loads(args.snapshot.read_text()), {
                "logical_parameters": args.logical_parameters,
                "weight_bits": args.weight_bits,
                "kv_bits": args.kv_bits,
                "context_tokens": args.context_tokens,
            })
            print(json.dumps(result, indent=2, sort_keys=True))
    except (ValueError, KeyError, OSError) as error:
        parser.exit(1, f"model profile failed: {error}\n")


if __name__ == "__main__":
    main()

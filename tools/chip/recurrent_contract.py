#!/usr/bin/env python3
"""Extract a pinned recurrent-state contract without importing model code.

Primary implementation files must match exact recorded hashes. This describes
one inspected fallback, not an executed checkpoint or fused-kernel certification.
"""

import argparse
import ast
import hashlib
import json
from pathlib import Path
import urllib.request

from model_profile import config_digest

MODEL = "Qwen/Qwen3.8-27B"
REVISION = "1d4bf0f2ff6012fd82039f2fa52739d0dd7c60c0"
CONFIG_SHA256 = "137754ebd46991bec0c0d9e660dc98256be0b20f0e825b28c95c31de6142d041"
IMPLEMENTATION = "c633787a7acb9bd11e57cd8d599be2b767bbdbaa"
MAX_SOURCE = 2_000_000
SOURCES = {
    "modeling_qwen3_5.py": {
        "path": "src/transformers/models/qwen3_5/modeling_qwen3_5.py",
        "sha256": "aac2a1bcca88829afc6dbdf83f97155ebf64d800ee9d703af3802a9aebcfcd18",
        "symbols": ["torch_recurrent_gated_delta_rule", "l2norm", "Qwen3_5GatedDeltaNet", "Qwen3_5RMSNormGated"],
    },
    "cache_utils.py": {
        "path": "src/transformers/cache_utils.py",
        "sha256": "2d2ab17aa02c9d2a993587ec58d1745b03595b6028c401cc6c976bb720ad6638",
        "symbols": ["LinearAttentionLayer"],
    },
}


def source_url(source):
    return f"https://raw.githubusercontent.com/huggingface/transformers/{IMPLEMENTATION}/{source['path']}"


def audit_source(name, raw):
    expected = SOURCES[name]
    if len(raw) > MAX_SOURCE or hashlib.sha256(raw).hexdigest() != expected["sha256"]:
        raise ValueError("primary source identity mismatch")
    tree = ast.parse(raw)
    symbols = {node.name: {"start_line": node.lineno, "end_line": node.end_lineno}
               for node in tree.body if isinstance(node, (ast.FunctionDef, ast.ClassDef))}
    if not all(symbol in symbols for symbol in expected["symbols"]):
        raise ValueError("inspected implementation symbols missing")
    return {"source_url": source_url(expected), "sha256": expected["sha256"],
            "symbols": {symbol: symbols[symbol] for symbol in expected["symbols"]}}


def state_contract(snapshot, batch=1, conv_element_bytes=2):
    if (not isinstance(snapshot, dict) or snapshot.get("model") != MODEL
            or snapshot.get("revision") != REVISION or type(snapshot.get("schema_version")) is not int
            or snapshot["schema_version"] != 1):
        raise ValueError("requires the pinned target snapshot")
    config = snapshot.get("config")
    if (not isinstance(config, dict) or snapshot.get("config_sha256") != CONFIG_SHA256
            or config_digest(config) != CONFIG_SHA256):
        raise ValueError("configuration identity mismatch")
    base = f"https://huggingface.co/{MODEL}/resolve/{REVISION}/"
    if snapshot.get("config_url") != base + "config.json":
        raise ValueError("configuration URL mismatch")
    if type(batch) is not int or not 1 <= batch <= 1024:
        raise ValueError("invalid batch size")
    if type(conv_element_bytes) is not int or conv_element_bytes not in (2, 4):
        raise ValueError("convolution precision must be explicit: 2 or 4 bytes")
    text = config.get("text_config")
    if not isinstance(text, dict) or text.get("model_type") != "qwen3_5_text":
        raise ValueError("unsupported architecture")
    expected = {"num_hidden_layers": 64, "linear_num_key_heads": 16, "linear_num_value_heads": 48,
                "linear_key_head_dim": 128, "linear_value_head_dim": 128,
                "linear_conv_kernel_dim": 4, "hidden_size": 5120}
    if any(type(text.get(key)) is not int or text[key] != value for key, value in expected.items()):
        raise ValueError("target dimensions changed")
    kinds = text.get("layer_types")
    if kinds != ["linear_attention", "linear_attention", "linear_attention", "full_attention"] * 16:
        raise ValueError("explicit hybrid layer schedule required")
    layers = [index for index, kind in enumerate(kinds) if kind == "linear_attention"]
    channels = 2 * 16 * 128 + 48 * 128
    return {
        "schema_version": 1, "evidence": "inspected_recurrent_operator_contract",
        "model": MODEL, "revision": REVISION, "config_sha256": snapshot["config_sha256"],
        "implementation_revision": IMPLEMENTATION, "batch": batch, "recurrent_layer_indices": layers,
        "per_layer_state_shape": [batch, 48, 128, 128], "recurrent_state_dtype": "FP32",
        "all_recurrent_state_bytes": batch * len(layers) * 48 * 128 * 128 * 4,
        "query_key_head_repeat": 3,
        "per_layer_conv_cache_shape": [batch, channels, 4],
        "conv_cache_element_bytes_assumption": conv_element_bytes,
        "all_conv_cache_bytes_under_assumption": batch * len(layers) * channels * 4 * conv_element_bytes,
        "operator_boundary": {
            "query": "L2 normalized (eps=1e-6), then divided by sqrt(key_dim)",
            "key": "L2 normalized (eps=1e-6)",
            "decay": "exp(g), g=-exp(A_log.float())*softplus(a.float()+dt_bias)",
            "beta": "sigmoid(b) in projection dtype, then cast to FP32 by fallback",
            "ordering": ["decay old state", "read decayed state with key", "beta-scaled value residual",
                         "key-residual outer-product update", "read updated state with query"],
        },
        "quantization_scope": {
            "mlp_weights": "experimental symmetric W4; only bounded sample demonstrated",
            "recurrent_state": "retain source FP32 baseline; reduced precision unvalidated",
            "qkv_and_output_projections": "no precision selected; calibration and model evaluation required",
            "conv_cache": "storage assumption only; runtime follows projected-input dtype",
            "decay_and_normalization": "source FP32 baseline; nonlinear hardware accuracy unimplemented",
        },
        "limitations": [
            "Configuration dimensions plus an inspected family fallback are not an executed checkpoint trace.",
            "FP32 state storage is sourced from fallback casts/allocation, not a measured device allocation.",
            "Convolution storage assumes the supplied element width and excludes extra history for recorded past.",
            "Independent rational core omits IEEE-754 rounding and nonlinear/preprocessing operators.",
            "Chunked prefill and substituted hub/FLA kernels have not been compared for equivalence.",
            "Full attention, projections, convolution, gated normalization, tokenizer and full-model execution remain unimplemented.",
            "No reduced recurrent-state precision, coding quality, physical PPA or manufacturability is validated.",
        ],
    }


def extract(snapshot, source_dir, batch=1, conv_element_bytes=2):
    result = state_contract(snapshot, batch, conv_element_bytes)
    sources = {}
    for name in SOURCES:
        with (source_dir / name).open("rb") as source:
            raw = source.read(MAX_SOURCE + 1)
        sources[name] = audit_source(name, raw)
    result["primary_sources"] = sources
    result["extractor_sha256"] = hashlib.sha256(Path(__file__).read_bytes()).hexdigest()
    reference = Path(__file__).resolve().parents[2] / "hardware/llm-asic/recurrent_reference.py"
    result["mathematical_reference_sha256"] = hashlib.sha256(reference.read_bytes()).hexdigest()
    return result


def fetch(source_dir):
    """Download only two pinned Python source files; never import or execute them."""
    source_dir.mkdir(parents=True, exist_ok=False)
    for name, expected in SOURCES.items():
        with urllib.request.urlopen(source_url(expected), timeout=30) as source:
            raw = source.read(MAX_SOURCE + 1)
        audit_source(name, raw)
        (source_dir / name).write_bytes(raw)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("snapshot", type=Path)
    parser.add_argument("--source-dir", type=Path, required=True)
    parser.add_argument("--fetch", action="store_true")
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--batch", type=int, default=1)
    parser.add_argument("--conv-element-bytes", type=int, default=2)
    args = parser.parse_args()
    try:
        with args.snapshot.open("rb") as source:
            raw = source.read(MAX_SOURCE + 1)
        if len(raw) > MAX_SOURCE:
            raise ValueError("snapshot exceeds bound")
        snapshot = json.loads(raw)
        # Reject incompatible configuration before any network operation.
        state_contract(snapshot, args.batch, args.conv_element_bytes)
        if args.fetch:
            fetch(args.source_dir)
        result = extract(snapshot, args.source_dir, args.batch, args.conv_element_bytes)
        args.out.parent.mkdir(parents=True, exist_ok=True)
        with args.out.open("x") as target:
            json.dump(result, target, sort_keys=True, indent=2)
            target.write("\n")
        print(json.dumps({"out": str(args.out), "evidence": result["evidence"],
                          "all_recurrent_state_bytes": result["all_recurrent_state_bytes"]}))
    except (ValueError, OSError, KeyError) as error:
        parser.exit(1, f"recurrent contract failed: {error}\n")


if __name__ == "__main__":
    main()

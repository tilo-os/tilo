#!/usr/bin/env python3
"""Proposed finite W4/A8 -> FP32 -> BF16 linear arithmetic contract.

Exact rationals implement each specified rounding boundary. This is neither a
calibrated quantizer nor a claim about a framework's GEMM/FMA evaluation order.
"""

import argparse
from fractions import Fraction
import hashlib
import json
from pathlib import Path
import sys

MAX_INPUTS = 5120
MAX_ROWS = 8
GROUP_SIZE = 128
CONTRACT = {
    "version": 1,
    "proposed_not_calibrated": True,
    "weight_codes": "existing exact absmax/7 symmetric [-7,7] compiler codes",
    "activation_codes": "per token/group absmax/127 symmetric [-127,127], nearest ties even, zero scale1",
    "scale_storage": "FP32 RNE from exact positive rational; positive underflow rejected",
    "rescaling": "FP32 RNE(weightScale*activationScale), RNE(float32(partial)*jointScale), ordered RNE accumulator add",
    "output_storage": "BF16 RNE from final FP32; finite signed zero and gradual underflow preserved",
    "nonfinite_policy": "reject source NaN/infinity and any FP32/BF16 overflow; do not clamp",
    "fma_or_reassociation": "not permitted",
    "activation_provenance": "caller supplied BF16 words; no activation capture/calibration proof",
}


def exact(value):
    if type(value) not in (int, Fraction):
        raise ValueError("exact integer/Fraction required")
    return Fraction(value)


def power_two(exponent):
    return Fraction(1 << exponent) if exponent >= 0 else Fraction(1, 1 << -exponent)


def finite_value(bits, fraction_bits):
    if fraction_bits not in (7, 23):
        raise ValueError("only BF16 and FP32 formats are supported")
    width = fraction_bits + 9
    if type(bits) is not int or not 0 <= bits < 1 << width:
        raise ValueError("invalid floating-point bit pattern")
    exponent = (bits >> fraction_bits) & 255
    significand = bits & ((1 << fraction_bits) - 1)
    if exponent == 255:
        raise ValueError("non-finite input is outside numerical contract")
    if exponent:
        significand += 1 << fraction_bits
        shift = exponent - 127 - fraction_bits
    else:
        shift = -126 - fraction_bits
    value = significand * power_two(shift)
    return -value if bits >> (width - 1) else value


def round_bits(value, fraction_bits):
    """Finite binary format RNE, gradual underflow; overflow is an error."""
    if fraction_bits not in (7, 23):
        raise ValueError("only BF16 and FP32 formats are supported")
    value = exact(value)
    sign = int(value < 0) << (fraction_bits + 8)
    magnitude = abs(value)
    if not magnitude:
        return 0
    exponent = magnitude.numerator.bit_length() - magnitude.denominator.bit_length()
    if magnitude < power_two(exponent):
        exponent -= 1
    quantum = power_two(max(exponent, -126) - fraction_bits)
    significand = round(magnitude / quantum)  # Fraction implements exact ties-even.
    if exponent < -126:
        return sign | significand  # Includes zero and carry into minimum normal.
    if significand == 1 << (fraction_bits + 1):
        significand >>= 1
        exponent += 1
    if exponent > 127:
        raise FloatingPointError("finite numerical contract overflow")
    return sign | ((exponent + 127) << fraction_bits) | (significand - (1 << fraction_bits))


def fp32_add(left, right):
    a, b = finite_value(left, 23), finite_value(right, 23)
    if a + b == 0:
        # Under RNE, exact cancellation is +0; -0 + -0 is -0.
        return (left & right) & 0x80000000 if not a and not b else 0
    return round_bits(a + b, 23)


def fp32_multiply(left, right):
    a, b = finite_value(left, 23), finite_value(right, 23)
    if not a or not b:
        return (left ^ right) & 0x80000000
    return round_bits(a * b, 23)


def bf16_output(fp32):
    value = finite_value(fp32, 23)
    return (fp32 >> 16) & 0x8000 if not value else round_bits(value, 7)


def positive_scale(value):
    value = exact(value)
    if value <= 0:
        raise ValueError("group scale must be positive")
    bits = round_bits(value, 23)
    if not finite_value(bits, 23):
        raise ValueError("positive group scale underflows FP32 storage")
    return bits


def quantize_activations(bf16):
    if not 1 <= len(bf16) <= GROUP_SIZE:
        raise ValueError("activation group must contain 1..128 BF16 values")
    values = [finite_value(bits, 7) for bits in bf16]
    maximum = max(map(abs, values))
    scale = maximum / 127 if maximum else Fraction(1)
    integers = [max(-127, min(127, round(value / scale))) for value in values]
    return integers, scale, positive_scale(scale)


def activation_code_integer(bits, maximum_bits):
    """Exact BF16 ratio using at most a 15-bit numerator/16-bit divisor.

    Candidate frontend arithmetic, independently checkable against the
    Fraction quantizer. The caller supplies the actual group's absmax word.
    """
    finite_value(bits, 7)
    finite_value(maximum_bits, 7)
    magnitude, maximum = bits & 0x7fff, maximum_bits & 0x7fff
    if maximum_bits & 0x8000 or magnitude > maximum:
        raise ValueError("maximum must be nonnegative and bound the activation")
    if not maximum or not magnitude:
        return 0
    exponent, max_exponent = magnitude >> 7, maximum >> 7
    mantissa = (magnitude & 127) + (128 if exponent else 0)
    max_mantissa = (maximum & 127) + (128 if max_exponent else 0)
    shift = max(max_exponent, 1) - max(exponent, 1)
    if shift >= 9:
        return 0
    numerator, denominator = 127 * mantissa, max_mantissa << shift
    quotient, remainder = divmod(numerator, denominator)
    if 2 * remainder > denominator or (2 * remainder == denominator and quotient & 1):
        quotient += 1
    return -quotient if bits & 0x8000 else quotient


def rescale_partials(partials, weight_scales, activation_scales):
    """Ordered groups: scale product, partial product, then accumulator add."""
    if not 1 <= len(partials) <= 40 or len(weight_scales) != len(partials) or len(activation_scales) != len(partials):
        raise ValueError("one positive weight/activation scale per group required")
    accumulator, contributions = 0, []
    for partial, weight, activation in zip(partials, weight_scales, activation_scales):
        if type(partial) is not int or not -(1 << 31) <= partial < 1 << 31:
            raise ValueError("partial must be signed INT32")
        if finite_value(weight, 23) <= 0 or finite_value(activation, 23) <= 0:
            raise ValueError("stored scales must be finite positive FP32")
        joint = fp32_multiply(weight, activation)
        contribution = fp32_multiply(round_bits(partial, 23), joint)
        contributions.append(contribution)
        accumulator = fp32_add(accumulator, contribution)
    return accumulator, bf16_output(accumulator), contributions


def linear_projection(activations_bf16, weights, exact_weight_scales, group_size=GROUP_SIZE):
    """One token, <=8 rows, <=5120 inputs; proposed blockwise activation W4A8."""
    if (type(group_size) is not int or not 1 <= group_size <= GROUP_SIZE
            or not 1 <= len(activations_bf16) <= MAX_INPUTS or not 1 <= len(weights) <= MAX_ROWS):
        raise ValueError("unsupported bounded projection shape")
    groups = (len(activations_bf16) + group_size - 1) // group_size
    if groups > 40 or len(exact_weight_scales) != len(weights):
        raise ValueError("unsupported group count or scale rows")
    quantized, scales, activation_bits = [], [], []
    for start in range(0, len(activations_bf16), group_size):
        q, scale, bits = quantize_activations(activations_bf16[start:start + group_size])
        quantized.extend(q)
        scales.append({"numerator": scale.numerator, "denominator": scale.denominator})
        activation_bits.append(bits)
    rows, partials, weight_bits, contribution_bits = [], [], [], []
    for row, row_scales in zip(weights, exact_weight_scales):
        if (len(row) != len(quantized) or len(row_scales) != groups
                or any(type(weight) is not int or not -7 <= weight <= 7 for weight in row)):
            raise ValueError("W4 row/scale shape or symmetric coefficient range invalid")
        row_partials = [sum(a * w for a, w in zip(quantized[start:start + group_size], row[start:start + group_size]))
                        for start in range(0, len(quantized), group_size)]
        stored = [positive_scale(scale) for scale in row_scales]
        fp32, bf16, contributions = rescale_partials(row_partials, stored, activation_bits)
        rows.append({"fp32_bits": fp32, "bf16_bits": bf16})
        partials.append(row_partials)
        weight_bits.append(stored)
        contribution_bits.append(contributions)
    return {"outputs": rows, "activation_integers": quantized,
            "activation_scales_exact": scales, "activation_scale_fp32_bits": activation_bits,
            "weight_scale_fp32_bits": weight_bits, "integer_partials": partials,
            "contribution_fp32_bits": contribution_bits}


def sample_projection(raw_manifest, artifacts, activations_bf16):
    # Import only repository-owned validation code; no downloaded model code.
    tools_path = str(Path(__file__).resolve().parents[2] / "tools/chip")
    if tools_path not in sys.path:
        sys.path.insert(0, tools_path)
    from weight_tile_artifact import validate_artifacts
    manifest = validate_artifacts(raw_manifest, artifacts)
    if len(activations_bf16) != manifest["columns"]:
        raise ValueError("activation count must match validated sample columns")
    vectors = json.loads(artifacts["vectors.json"])
    scales = [[Fraction(entry["numerator"], entry["denominator"])]
              for entry in manifest["quantization"]["group_scales_exact"]]
    result = linear_projection(activations_bf16, vectors["weights"], scales)
    a = [finite_value(bits, 7) for bits in activations_bf16]
    errors = []
    for decoded, output in zip(manifest["decoded_exact"], result["outputs"]):
        source = sum(value * Fraction(weight["numerator"], weight["denominator"]) for value, weight in zip(a, decoded))
        error = finite_value(output["bf16_bits"], 7) - source
        errors.append({"source_dot_exact": {"numerator": source.numerator, "denominator": source.denominator},
                       "bf16_error_exact": {"numerator": error.numerator, "denominator": error.denominator}})
    result["sample_source"] = {key: manifest[key] for key in
                               ["model", "revision", "config_sha256", "index_canonical_sha256", "header_sha256", "source_url", "tensor", "first_row", "first_column", "rows", "columns"]}
    result["sample_artifact_sha256"] = manifest["artifact_sha256"]
    result["source_weight_scales_exact"] = manifest["quantization"]["group_scales_exact"]
    result["source_dot_comparison"] = errors
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--sample", type=Path, required=True)
    parser.add_argument("--activations", type=Path, required=True, help="JSON list of BF16 unsigned 16-bit storage words")
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()
    # Immutable new receipt, including validation failure; no weight network.
    with args.out.open("x") as output:
        result = {"schema_version": 1, "evidence": "proposed_group_scaled_linear_reference", "status": "running",
                  "reference_sha256": hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
                  "artifact_validator_sha256": hashlib.sha256((Path(__file__).resolve().parents[2] / "tools/chip/weight_tile_artifact.py").read_bytes()).hexdigest(),
                  "contract": CONTRACT,
                  "excludes": ["activation_calibration", "model_quality", "framework_gemm_equivalence", "hardware_scaler", "full_layer_execution", "offline_checkpoint_payload_authentication"]}
        try:
            def read(path):
                with path.open("rb") as stream:
                    data = stream.read(8 * 1024 * 1024 + 1)
                if len(data) > 8 * 1024 * 1024:
                    raise ValueError("reference input exceeds byte bound")
                return data
            raw = read(args.sample / "sample.json")
            artifacts = {name: read(args.sample / name) for name in
                         ["sample.bf16", "fixed_linear_fixture.sv", "tb.sv", "vectors.json"]}
            activation_raw = read(args.activations)
            result["input_sha256"] = {**{name: hashlib.sha256(data).hexdigest() for name, data in artifacts.items()},
                                      "sample.json": hashlib.sha256(raw).hexdigest(),
                                      "activations.json": hashlib.sha256(activation_raw).hexdigest()}
            result["projection"] = sample_projection(raw, artifacts, json.loads(activation_raw))
            result["status"] = "passed"
        except BaseException as error:
            result["status"] = "cancelled" if isinstance(error, KeyboardInterrupt) else "failed"
            result["failure"] = str(error)
            raise
        finally:
            json.dump(result, output, indent=2, sort_keys=True)
            output.write("\n")


if __name__ == "__main__":
    main()

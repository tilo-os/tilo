"""Exact signed W4A8 group dot products; no quantization or quality claim.

The independent oracle uses unbounded Python integers and explicit ranges.
Outputs are INT32 partial sums before weight/activation group scales.
"""

MAX_GROUP_SIZE = 5120
MAX_LANES = 64


def signed_integer(value, lower, upper, name):
    if type(value) is not int or not lower <= value <= upper:
        raise ValueError(f"{name} must be an integer in [{lower}, {upper}]")
    return value


def group_dot(activations, weights):
    """One group: weights[row][input], returning one exact partial per row."""
    if not 1 <= len(activations) <= MAX_GROUP_SIZE:
        raise ValueError("unsupported activation group length")
    if not 1 <= len(weights) <= MAX_LANES:
        raise ValueError("unsupported output lane count")
    inputs = [signed_integer(value, -128, 127, "activation") for value in activations]
    result = []
    for row in weights:
        if len(row) != len(inputs):
            raise ValueError("coefficient row and input length disagree")
        coefficients = [signed_integer(value, -8, 7, "weight") for value in row]
        total = sum(value * coefficient for value, coefficient in zip(inputs, coefficients))
        signed_integer(total, -(1 << 31), (1 << 31) - 1, "partial sum")
        result.append(total)
    return result

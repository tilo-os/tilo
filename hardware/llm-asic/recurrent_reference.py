"""Exact mathematical gated-delta core; not an FP32 runtime emulator.

One head, one token. Query/key normalization, query head-dimension scaling,
exp(g), sigmoid(beta logits), projections and convolution occur upstream.
Inputs here are exact prepared rational values. The reference preserves update
ordering so a future finite-precision implementation has an independent oracle.
"""

from fractions import Fraction

MAX_DIMENSION = 128


def exact(value):
    if type(value) not in (int, Fraction):
        raise ValueError("exact integers or Fractions required; floating-point rounding is unspecified")
    return Fraction(value)


def vector(values, size):
    if len(values) != size:
        raise ValueError("vector dimension disagrees with recurrent state")
    return [exact(value) for value in values]


def gated_delta_step(state, query, key, value, decay, beta):
    """Return a NEW [key,value] state matrix and post-update query readout."""
    keys = len(state)
    if not 1 <= keys <= MAX_DIMENSION:
        raise ValueError("unsupported key dimension")
    values = len(state[0])
    if not 1 <= values <= MAX_DIMENSION or any(len(row) != values for row in state):
        raise ValueError("invalid value dimension or ragged recurrent state")
    q, k, v = vector(query, keys), vector(key, keys), vector(value, values)
    rho, coefficient = exact(decay), exact(beta)
    if not 0 <= rho <= 1 or not 0 <= coefficient <= 1:
        raise ValueError("decay and beta must lie in [0,1]")
    decayed = [[exact(entry) * rho for entry in row] for row in state]
    predicted = [sum(k[i] * decayed[i][j] for i in range(keys)) for j in range(values)]
    correction = [coefficient * (v[j] - predicted[j]) for j in range(values)]
    updated = [[decayed[i][j] + k[i] * correction[j] for j in range(values)] for i in range(keys)]
    output = [sum(q[i] * updated[i][j] for i in range(keys)) for j in range(values)]
    return updated, output

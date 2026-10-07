# Architecture exploration contract

The initial target is the text path of the pinned Qwen3.8-27B configuration in
[model selection](model-selection.md). This is an exploration specification,
not a fabrication checkpoint or a measured performance claim. The complete
checkpoint also contains vision and multi-token-prediction tensors; a text-only
product must explicitly establish which components its reference runtime uses.

## Product workload

Optimize complete coding-agent task time, with batch-one interactive decoding,
repository context and tool calls. Evaluate prefill separately from decoding.
Use the common coding task contract to determine context lengths, useful
reasoning budgets and acceptable quality loss before selecting precision.
An isolated tokens-per-second benchmark cannot establish the product's value.

Keep architecture and checkpoint binding separate. Compare fully fixed
weights, replaceable weight modules and programmable weight storage using the
same measured quality and system-cost requirements. A current checkpoint's
October 2028 usefulness remains unproven. Adapter support alone does not prove
that obsolete base-model capability can be recovered.

## First computation slice

The configuration describes 64 blocks with hidden width 5,120 and feed-forward
width 17,408. The three gate/up/down projection matrices therefore describe
`64 * 3 * 5120 * 17408 = 17,112,760,320` coefficient positions. This is a
configuration-derived matrix count, not a traced executable graph.

Start with a fixed-coefficient, signed W4A8 feed-forward group projection. A
tile consumes one activation per accepted cycle and produces an INT32 partial
dot product for each output lane. Different quantization groups retain separate
scales: their integer partial sums must not be added without rescaling.
Synthetic coefficients establish arithmetic and protocol behavior only;
checkpoint-derived quantization and complete-agent quality remain separate
experiments.

Compare a coefficient-selection implementation with generated constant
arithmetic after correctness is established. Record actual synthesized
netlists and mapped results. Generic cell counts are not physical area, power
or timing. A technology library, constraints and physical implementation are
required before making those claims.

## State and memory are architecture constraints

The captured configuration combines 48 linear-attention blocks and 16 full
attention blocks. The existing analytical profiler computes only the ordinary
full-attention K/V storage component. At batch one, 32,768 tokens and assumed
16-bit K/V, that component is 2 GiB; at 262,144 tokens it is 16 GiB. Neither
number includes recurrent state, convolutions, allocator overhead or workspace.

For an implementation that reads that whole cache once per decoded token,
1,000 tokens/second would imply approximately 2 TiB/second of cache reads at
32K context. This is conditional arithmetic, not an achievable device rate or
a validated access trace. It makes cache layout, grouped-query reuse,
quantization and context policy first-class experiments even when weight
traffic is eliminated.

The recurrent-state update also requires its own exact runtime trace and
numerical reference. It cannot be replaced with an ordinary attention kernel
or omitted from the storage/power budget. Sampling, normalization, embeddings,
nonlinearities, tokenizer and host/tool interaction remain part of full-system
execution.

## Decisions required before scaling

1. Pin the reference runtime and trace prefill/decode operators, state shapes,
   layouts and accesses against real weights and tokenizer artifacts.
2. Establish useful coding outcomes for the unquantized reference and each
   proposed weight, activation, cache and recurrent-state format.
3. Verify compiled tiles and state updates against independent references,
   including stalls, resets and group boundaries; then verify complete layers.
4. Map representative fixed-weight and programmable blocks under the same
   process constraints, including storage, interconnect and interfaces.
5. Obtain process/IP, packaging, test and fabrication quotes for the resulting
   requirements. No manufacturing cost or fabrication access is established.

The $100,000 seed is not treated as a complete-chip fabrication budget. It buys
progress toward these decisions; the capital and partners required to produce
the final ASIC remain unresolved.

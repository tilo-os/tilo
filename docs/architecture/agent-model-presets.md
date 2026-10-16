# Host-side planning request presets

A host operator using the existing `openai:<base url>` planner can opt in to
request hints with `TILO_AGENT_PRESET=fastest|balanced|cheapest`.
No preset is selected by default. This does not change the Settings-selected
model gateway, scripted planners, agent admission, grants or confirmation.
Provider credentials remain in the existing host-side caller.

| Model class | fastest | balanced | cheapest |
| --- | --- | --- | --- |
| small, medium, large | priority | provider default | Flex |
| very large | Flex | Flex | Flex |

Every preset requests `low` reasoning effort. These are preferences, not
capability detection, latency promises or price estimates. In particular,
`fastest` on a very large model still requests Flex. A provider may reject
these fields or assign them different semantics. There is no tier fallback
and no request hedging: selecting a preset never duplicates a paid request.

`TILO_AGENT_MODEL_CLASS=small|medium|large|very-large` overrides the heuristic
based on family names. Unknown model names default to medium. Provider aliases
should use an explicit class rather than relying on a name to describe price
or capabilities.

`TILO_AGENT_SERVICE_TIER=auto|default|flex|priority` and
`TILO_AGENT_REASONING_EFFORT=none|minimal|low|medium|high|xhigh` override the
individual hints, with or without a preset. Values are case-insensitive and
trimmed. An invalid explicit value suppresses that hint; an invalid preset
selects none. `default` explicitly requests the default tier instead of a
preset's priority or Flex tier.

Hints are added before the target's existing request shaping. The Cloud
metered target therefore strips both fields under its closed allowlist,
retains its output cap and uses the existing idempotency-key retry contract.
The model's tool catalogue, step bound, repair handling and invocation path
are unchanged. Tests use loopback HTTP fixtures, not provider accounts.

## Context and authority remain separate

Request hints do not authorise automatic record creation, discovery reads,
new exports or attached-view effects. Nor do they authorise sending additional
record contents to a model provider. Such changes need their own review of
context selection, provider disclosure, typed agent identity, confinement and
current broker authority. The access model in [agents.md](agents.md) and the
existing certified caller APIs remain the source of authority.

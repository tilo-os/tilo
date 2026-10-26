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

## A second model for coordinated tasks' items

With `TILO_AGENT_DECOMPOSE` on, a coordinator lists a task's items and each
item is planned by a fresh agent. `TILO_AGENT_DECOMPOSE_ITEM_MODEL=<model id>`
plans the items with another model, while the coordinator's turns (the list,
news, an item not done, the check) and whole tasks stay with the agent's own:
a strong model coordinating cheap ones. A `claude-` model is asked at
Anthropic's Messages API and any other at OpenAI's, unless
`TILO_AGENT_DECOMPOSE_ITEM_URL` names the endpoint (`openai:`, `anthropic:`
or `cloud:<base url>`). Its key is its own,
`TILO_AGENT_DECOMPOSE_ITEM_API_KEY` or `TILO_AGENT_DECOMPOSE_ITEM_API_KEY_FILE`;
the host refuses to start when it is missing, or its file is not there,
unless an `openai:` endpoint was named. `TILO_AGENT_DECOMPOSE_ITEM_PRESET`
takes the place of `TILO_AGENT_PRESET` for it (Anthropic takes none of the
hints), and its class comes from its own name, never from
`TILO_AGENT_MODEL_CLASS`. The usage log names each call's model beside its
role and item, so a run's cost is summed per model.

## Context and authority remain separate

Request hints do not authorise automatic record creation, discovery reads,
new exports or attached-view effects. Nor do they authorise sending additional
record contents to a model provider. Such changes need their own review of
context selection, provider disclosure, typed agent identity, confinement and
current broker authority. The access model in [agents.md](agents.md) and the
existing certified caller APIs remain the source of authority.

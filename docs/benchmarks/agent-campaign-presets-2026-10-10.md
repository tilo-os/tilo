# Agent model presets on the campaign benchmark, 2026-10-10

`TILO_AGENT_PRESET` (`fastest`, `balanced`, `cheapest`) sets a planning
model's service tier, reasoning effort and hedging for its class
(`tilo_agent_participant::profile`). This note records the runs the presets
were set from.

## Method

The workload is `crates/agent/agent-participant/tests/campaign_bench.rs`: six
agents in one room run eight dependent tasks over Canvas, Sheets, Notes and
Writer. The tasks are an adjusted 1520×855 PNG with a teal panel, a headline
workbook with formulas, a note, a launch page with the picture, a fact check,
one sheet per agent, and DOCX, XLSX and Markdown exports. An external grader
checks every artefact by content (image alignment, panel position and colour,
cell values, headings, facts, the embedded picture).

- **Time:** first task issued to last task done, without room setup. The
  median of five runs.
- **Cost:** list prices per token, with cached input and cache writes priced
  apart. Flex is 0.5× and priority 2×. The median of runs 2–5, once the
  prompt caches are warm.
- **Runs:** macOS (Apple silicon), one frozen test binary per batch.
- **Terra's price is assumed:** GPT-5.6 Terra is not on the current pricing
  page, so it is priced at $2 in and $12 out per million tokens.

For scale, Codex driving the same campaign through macOS computer use on
GPT-6 Astra took 1504 s and cost $17.70.

## OpenAI, one row per preset

| Model | Class | Preset | Time | Cost |
| --- | --- | --- | --- | --- |
| GPT-6 Luna | small | fastest (priority, hedge 3.5 s) | 6.8 s | $0.0042 |
| GPT-6 Luna | small | balanced (standard) | 10.6 s | $0.0023 |
| GPT-6 Luna | small | cheapest (Flex) | 22.3 s | $0.0009 |
| GPT-6 Sol | medium | fastest (priority) | 6.7 s | $0.077 |
| GPT-6 Sol | medium | balanced | 8.4 s | $0.037 |
| GPT-6 Sol | medium | cheapest | 7.1 s | $0.019 |
| GPT-6.1 Sol | medium | fastest | 9.9 s | $0.073 |
| GPT-6.1 Sol | medium | balanced | 11.6 s | $0.037 |
| GPT-6.1 Sol | medium | cheapest | 26.9 s | $0.027 |
| GPT-5.6 Terra | medium | fastest | 8.9 s | $0.106 |
| GPT-5.6 Terra | medium | balanced | 10.2 s | $0.052 |
| GPT-5.6 Terra | medium | cheapest | 10.1 s | $0.027 |
| GPT-6 Astra | very large | fastest (Flex) | 10.2 s | $0.101 |
| GPT-6 Astra | very large | balanced (Flex) | 9.9 s | $0.100 |
| GPT-6 Astra | very large | cheapest (Flex) | 9.9 s | $0.099 |

Two of the 75 runs in this table failed a check, one on Terra `balanced` and
one on Terra priority with no hedge; the rest passed every check.

## Choosing the hedge

A hedged call that has not answered by the hedge delay is sent again, and the
first answer is taken. It pays for itself only past the knee of the model's
latency tail. Per-call latency at the standard or priority tier:

| Model | Median call | 90th percentile |
| --- | --- | --- |
| GPT-6 Luna (priority) | 1.9 s | 3.0 s |
| GPT-6 Sol (priority) | 2.0 s | 3.2 s |
| GPT-6.1 Sol (priority) | 2.8 s | 5.0 s |
| GPT-5.6 Terra (priority) | 2.4 s | 4.6 s |
| GPT-6 Astra (Flex) | 3.0 s | 4.7 s |

At the priority tier, by hedge delay:

| Model | Never | 3.5 s | 4 s | 4.5 s |
| --- | --- | --- | --- | --- |
| GPT-6 Sol | 6.7 s, $0.077 | — | 8.5 s, $0.076 | 7.9 s, $0.078 |
| GPT-6.1 Sol | 9.9 s, $0.073 | — | 10.2 s, $0.088 | 9.1 s, $0.078 |
| GPT-5.6 Terra | 8.9 s, $0.106 | 10.1 s, $0.145 | 8.6 s, $0.116 | 12.0 s, $0.150 |

A medium model's 90th-percentile call is within about two seconds of its
median. A hedge early enough to matter resends ordinary calls, and one late
enough not to rarely fires. Across the three models, never hedging was the
fastest on average (8.5 s, against 9.1 s at 4 s and 9.7 s at 4.5 s) and
always the cheapest. Run-to-run noise is about ±1.5 s at five runs.

So the medium and large `fastest` presets do not hedge. The small one hedges
after 3.5 s: GPT-6 Luna's tail is short, and a resent call costs a hundredth
of a cent. Very large models are not hedged either, because their calls are
too dear to send twice. `TILO_AGENT_HEDGE_MS` sets a hedge for any model, and
`0` turns a preset's hedge off.

## Notes per tier

- **Flex on GPT-6 Astra:** as fast as standard or priority at half and a
  quarter of the price, so every Astra preset uses it.
- **Flex on GPT-6.1 Sol:** about half its calls were answered
  `flex_unavailable`, and each was resent at the standard tier after about
  0.7 s. The Flex calls that did run took 8–16 s, so `cheapest` on that model
  is 26.9 s; `balanced` is the better pick there.
- **Flex on GPT-6 Luna:** the median call went from 2.2 s to 4.8 s; `cheapest`
  trades about 12 s for a 60% lower cost.
- **Reasoning effort:** `low` is the floor on the GPT-6 family (`none` and
  `minimal` are refused), and every preset uses it. On GPT-6 Luna at the
  standard tier, `low` against the default took the campaign from 34.9 s to
  9.0 s.

## Anthropic

The same campaign through `anthropic:https://api.anthropic.com/v1` at low
effort. The Messages API is stateless, so each turn resends the conversation;
cache breakpoints after the tools, the apps' reference and the latest turn
kept about 90% of the input tokens as cache reads. Anthropic has no Flex or
priority tier, so `fastest` and `balanced` differ only in hedging, and only
on a small model (`cheapest` equals `balanced`).

| Model | Class | Time | Cost | Median call | Passed |
| --- | --- | --- | --- | --- | --- |
| Claude Haiku 5.5 | small | 8.1 s | $0.0045 | 1.3 s | 5 of 10 |
| Claude Sonnet 5.5 | medium | 9.2 s | $0.058 | 2.3 s | 10 of 10 |
| Claude Opus 5.5 | large | 13.2 s | $0.114 | 3.6 s | 10 of 10 |
| Claude Fable 5.1 | very large | 16.2 s | $0.288 | 4.2 s | 10 of 10 |

Time, cost and median call are from the `balanced` runs; "passed" counts
the `fastest` and `balanced` runs together.

- **Haiku hedging:** its 90th-percentile call is 2.0 s, so the 3.5 s hedge
  never fired; `fastest` took 10.8 s against `balanced`'s 8.1 s, which is
  noise between identical setups.
- **Haiku failures:** every failed Haiku run drew the teal panel over about
  half the picture rather than its right 38%, covering the lamp. GPT-6 Luna,
  the same class, placed it right in all fifteen of its runs.
- **Sonnet, Opus and Fable refuse a forced tool call** (`tool_choice: any`).
  `AnthropicModel` forces one and, on that refusal, leaves the choice to the
  model from then on. That costs one failed request, under a second, per
  agent.
- **Fable's tail:** at 10 s, its 90th-percentile call is the longest
  measured. The campaign's critical path is four plans deep, so it took 1.6
  times GPT-6 Astra's time at 2.9 times its Flex cost.

# Spelling & grammar checking — findings + decision

Text checking must live in the shared `ui` block/text editor (build once, every
app gets it), not siloed in one app. Today only `word-processor` spell-checks
(the in-tree English `crates/text/spellcheck`); notes/mail/chat/sheets get
nothing, and there is no grammar checking anywhere.

We benchmarked the two Rust-native options by **actually executing them** on a
labelled corpus (planted German + English errors), not by reading docs. Scratch
crate: `.claude/experiments/grammar-bench/` (re-runnable).

## The decisive fact: Harper is English-only

`harper-core` 2.5.0 (Automattic, actively maintained) supports **only English
dialects** (US/UK/CA/AU/IN). There is no German or any other language — fed
German, it flags every word as misspelled. **It cannot serve a user base that is
mostly not day-to-day English speakers.**

`nlprule` 0.6.4 loads English/German/Spanish LanguageTool data — but it is
**unmaintained since 2021** (pinned to LanguageTool 5.2).

## Measured quality (planted errors, executed)

| | English detection | German detection | false positives |
|---|---|---|---|
| Harper | **79%**, excellent messages | — (unsupported) | 0% |
| nlprule | 50% (missed "she go", "a apple", "the the", "recieve") | **31%** | 0% |

nlprule's German is **hollow**: it missed every core case — der/das gender,
adjective endings ("ein guter Frau"), verb conjugation ("er gehen"), the spelling
"Fahradd", capitalization. It only catches confusion-words that exist as XML
patterns (das/dass, "nach Hause"), has **no spell dictionary**, and the real
German agreement engine is Java in LanguageTool and absent from the `.bin`.
"Supports German" = "loads a German model", not "usefully checks German".

## Measured performance (executed)

| | init | per-paragraph | peak RSS | on-disk data |
|---|---|---|---|---|
| Harper | ~500 ms | **0.8 ms** | 152 MB | **0** (embedded; 20 MB self-contained binary) |
| nlprule EN | ~285 ms | 7.4 ms (~9× slower) | 153 MB | 19 MB `.bin` (LGPL) |
| nlprule DE | ~345 ms | 4.1 ms | 190 MB | 13 MB `.bin` (LGPL) |
| nlprule en+de resident | — | — | ~300 MB | 31 MB |

Low-end (~1–2 GB RAM): Harper's latency margin is large; neither is "light"
(~150 MB floor); nlprule with two languages resident (~300 MB) is a real problem
on 1 GB, and ships 31 MB of LGPL rule blobs per two languages.

## Decision

There is **no single multilingual winner**, and nlprule's only advantage — that
it "runs" on German — is fake (31%, misses everything that matters). So:

1. **Spelling in ALL languages (incl. German) → hunspell/nuspell** with
   per-language dictionaries. This is the highest-value, genuinely multilingual
   layer, the Linux standard (dictionaries already packaged per language), tiny,
   deterministic. It replaces the siloed English `spellcheck` crate and gives
   every app spell-check **in the user's language**. **This is the part that
   matters for non-English users and it is the first thing to build.**
2. **English grammar → Harper** (fast, light, maintained, self-contained,
   zero data files). A bonus layer for English text; must never run on non-English
   content (route by the document/paragraph language).
3. **German / other-language *grammar* → deferred.** No acceptable local option
   exists today (nlprule hollow + abandoned; LanguageTool = heavy JVM service).
   Revisit only with a current LanguageTool-derived engine or a small local model,
   and only if it clears a real quality bar on German.

Explicitly: **we do not ship an English-only checker as "the" checker.** The
multilingual spelling layer (hunspell) is the requirement; Harper's English
grammar is additive, not the foundation.

## Integration

One `ui` task: wire hunspell (per-language dictionaries, red wavy squiggles,
right-click corrections, user dictionary) into the shared block/text editor, with
language detected per document/paragraph; layer Harper's grammar lints on English
runs only. Every text-bearing app inherits it — the same build-once pattern as the
DnD/palette/popover primitives.

# Spelling & grammar checking — findings + decision

Text checking lives in the shared `ui` text input so applications do not need
separate implementations. The in-tree `crates/text/spellcheck` provides the
current spelling layer; grammar checking remains future work.

The two Rust-native options were benchmarked on a labelled corpus with planted
German and English errors.

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

The shared TextInput baseline is now implemented: checking defaults on, follows
the session or field language, checks against the installed Hunspell
dictionaries in-process (parsed by `spellbook`; the launcher's system read
profile exposes `/usr/share/hunspell` and `/usr/share/myspell` to every
sandboxed app, so there is no spelling service or helper process) with an
embedded English word list, paints shaped/wrapped squiggles, and offers
corrections plus "Add to dictionary" on right-click. Once the word under the
caret has rested for a few hundred milliseconds — a pause at the end of a
word, or the caret moved back into an earlier one — a misspelled word shows
its best suggestion in a chip that a click applies. Untouched inactive
fields do no checking; edits are debounced. Structured fields opt out by semantic
`TextInputKind`, and shared search, number, code, secret, and editable-name
widgets set that intent automatically. Common offline dictionaries ship in the
OS image.

Structured fields can explicitly opt back in, and any field can pin its own
dictionary language independently of the session. Conservative autocorrection
applies only safe-listed/common typos and first-ranked adjacent transpositions
at Space, punctuation, Enter, or an equivalent IME commit. Settings exposes
separate checking and autocorrection switches; immediate Backspace and the
context menu restore an automatic change, while "Ignore in this field" sits
between a one-off false positive and the permanent personal dictionary.

Selected text also has an asynchronous action seam. A host registers labels such
as Translate, Rewrite, Explain, or Check grammar; choosing one yields the exact
text and byte range without running model work in the widget event handler. The
host must expose the same capability through its resource/action registry when
it is user-facing, so keyboard, accessibility, and automation do not depend on a
right-click.

Grammar is the next layer, not part of synchronous editing:

1. Run Harper only for English paragraphs on a worker after an idle debounce.
2. Tag every request/result with the document revision and discard stale results.
3. Return typed diagnostics (range, category, message, replacements), sharing
   the same decoration/context-action UI as spelling.
4. Keep the checker behind a small engine trait. A future multilingual local
   model can be a separate process with explicit download/RAM controls rather
   than becoming a mandatory dependency of every app.

That engine boundary is the credible OSS unit: corpus + evaluator, typed protocol,
Harper adapter, cancellation/staleness handling, and editor-agnostic diagnostics.
It should not bundle Tilo UI or CRDT types. Quality gates must be published per
language; unsupported language is a normal capability result, never a silent
English fallback.

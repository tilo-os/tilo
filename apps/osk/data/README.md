# OSK language data

Compact, embedded, **offline** English data for the on-screen keyboard's
swipe decoder, tap-target resolver and next-word predictor. Both tables are
`include_str!`-bundled into the `osk` binary, so the keyboard works with no
network and no model download. Personal vocabulary and word pairs are learned
on the device on top of this baseline.

## Files

| File             | What it is |
|------------------|------------|
| `words.txt`      | `word<TAB>weight`, the 30 000 most frequent SCOWL words, most frequent first. |
| `bigrams.txt`    | `w1<TAB>w2<TAB>weight`, the 40 000 most frequent word pairs whose words are both in `words.txt`. |
| `generate.py`    | Deterministic generator for both tables. |
| `LICENSE.SCOWL`  | SCOWL's copyright and permission notices, reproduced verbatim as its licence requires. |

`weight = round(log10(count) * 1000)`: a small integer that preserves the
frequency ordering and lets `dict::weight_to_count` recover an estimated count.
Ties are ordered alphabetically.

## Provenance

The *vocabulary* and the *frequencies* come from two separate sources:

1. **Which words exist — SCOWL 2020.12.07** (Spell Checker Oriented Word
   Lists, Kevin Atkinson, <http://wordlist.aspell.net/>), from
   `https://downloads.sourceforge.net/wordlist/scowl-2020.12.07.tar.gz`
   (SHA-256 `5587667c…fb859cc`). Lists used: `english-words`,
   `american-words`, `english-upper`, `american-upper` at sizes 10–60,
   keeping only plain `A–Z` words (no apostrophes, accents or abbreviations),
   lower-cased. Proper-name lists are excluded.
2. **How common they are — Google Books Ngram, English Fiction, version
   20200217** (<https://books.google.com/ngrams/>), the 1-gram file and all 47
   2-gram shards under
   `https://storage.googleapis.com/books/ngrams/books/20200217/eng-fiction/`.
   Match counts are summed over letter case for publication years 1980–2019;
   part-of-speech-tagged entries are ignored. Fiction is used because its
   dialogue is closer to what people type than the full corpus.

Every input is pinned by byte size and SHA-256 inside `generate.py`, and the
run aborts on any mismatch.

## Licences

The OSK is AGPL-3.0-only. Both inputs are permissive and only require their
notices to travel with the data:

* **SCOWL** — a permissive notice licence (Kevin Atkinson; its constituent
  lists are public domain or carry similar notice-only permissions, all
  reproduced in `LICENSE.SCOWL`). Sizes above 60 are not used, so the UK
  Advanced Cryptics Dictionary (size 80) is not included.
* **Google Books Ngram** — Creative Commons Attribution 3.0 Unported
  (<https://creativecommons.org/licenses/by/3.0/>). Attribution: *Google Books
  Ngram Viewer, <http://books.google.com/ngrams>; Jean-Baptiste Michel\*, Yuan
  Kui Shen, Aviva Presser Aiden, Adrian Veres, Matthew K. Gray, The Google
  Books Team, Joseph P. Pickett, Dale Hoiberg, Dan Clancy, Peter Norvig, Jon
  Orwant, Steven Pinker, Martin A. Nowak, and Erez Lieberman Aiden\*.
  Quantitative Analysis of Culture Using Millions of Digitized Books. Science
  (2010).* The tables here are an adaptation: counts are filtered to the SCOWL
  vocabulary, re-aggregated over case and years, truncated and log-scaled.

Earlier versions of these tables came from Peter Norvig's `count_1w.txt` /
`count_2w.txt`, which are derived from the LDC-licensed Google Web 1T corpus;
Norvig's MIT grant covers his code, not those counts, so they were replaced.

The optional FUTO swipe decoder (`src/futo.rs`) loads model weights from
`$TILO_SWIPE_MODELS` or `/usr/share/tilo/swipe-models` at run time. Nothing in
this repository or the OS image ships those weights; they are under the FUTO
Model Weights License 1.0, which is not an open-source licence and requires a
visible "powered by FUTO Swipe" notice to end users if they are distributed.

## Regenerating

From the repository root:

```sh
nice -n 19 ionice -c3 uv run apps/osk/data/generate.py
```

The run streams about 26 GB (nothing large is written to disk), uses the
Python standard library only, takes roughly half an hour and rewrites
`words.txt` and `bigrams.txt` in place. `--jobs N` sets how many 2-gram shards
are processed in parallel (default 2, each a CPU-bound process); `--out DIR`
writes elsewhere. Same inputs, byte-identical outputs.

# OSK language data

Compact, embedded, **offline** English and French data for the on-screen
keyboard's swipe decoder, tap-target resolver, autocorrector and next-word
predictor. Every table is `include_str!`-bundled into the `osk` binary, so the
keyboard works with no network and no model download. The letter layout picks
the language: QWERTY types English, AZERTY French. Personal vocabulary and
word pairs are learned on the device on top of this baseline.

## Files

| File                  | What it is |
|-----------------------|------------|
| `words-en.txt`        | `word<TAB>weight`, the 40 000 most frequent English words, most frequent first. |
| `bigrams-en.txt`      | `w1<TAB>w2<TAB>weight`, the 40 000 most frequent English word pairs whose words are both in `words-en.txt`. |
| `words-fr.txt`        | The 50 000 most frequent French words, same format. |
| `bigrams-fr.txt`      | The 40 000 most frequent French word pairs, same format. |
| `extra-en.txt`        | Everyday, chat and technical words SCOWL lacks (`wifi`, `github`, `wishlist`, `lol`, ...), added to the English vocabulary. |
| `generate.py`         | Deterministic generator for all four tables. |
| `LICENSE.wordfreq`    | wordfreq's licence and attribution notices. |
| `LICENSE.SCOWL`       | SCOWL's copyright and permission notices, reproduced verbatim as its licence requires. |
| `LICENSE.Grammalecte` | The Grammalecte lexicon's notice and the Mozilla Public License 2.0. |

`weight = round(log10(count) * 1000)`: a small integer that preserves the
frequency ordering and lets `dict::weight_to_count` recover an estimated count.
Ties are ordered alphabetically.

## Provenance

Each language combines three sources: one decides *which words exist*, one
*how common each word is*, and one *which word follows which*.

1. **Vocabulary.**
   * English — **SCOWL 2020.12.07** (Spell Checker Oriented Word Lists, Kevin
     Atkinson, <http://wordlist.aspell.net/>), from
     `https://downloads.sourceforge.net/wordlist/scowl-2020.12.07.tar.gz`.
     Lists used: `english-words`, `american-words`, `english-upper`,
     `american-upper` at sizes 10–60, keeping only plain `a–z` words (no
     apostrophes, accents or abbreviations), lower-cased. Proper-name lists
     are excluded. SCOWL predates much everyday vocabulary, so the words in
     `extra-en.txt` are added.
   * French — **Grammalecte lexicon 7.7** ("Lexique français, liste des
     formes fléchies", Olivier R. and the Dicollecte contributors,
     <https://grammalecte.net/>), from
     `https://www.grammalecte.net/dic/lexique-grammalecte-fr-v7.7.zip`: every
     inflected form of the database the Grammalecte Hunspell dictionaries are
     built from, except proper names (tags `npr`, `prn`), lower-cased and
     limited to `a–z` plus `àâäçéèêëîïôöùûüÿæœ`.
   * Both keep words of at most 24 letters and only the real one-letter
     words (`a`, `i`; `a`, `à`, `y`, `ô`), so stray letters and elided
     articles (`l'`, `d'`) never become suggestions.
2. **Word frequency — wordfreq 3.1.1** (Robyn Speer,
   <https://github.com/rspeer/wordfreq>), the `large_en` and `large_fr` lists
   from `wordfreq-3.1.1-py3-none-any.whl` on PyPI. wordfreq blends subtitles,
   web text, Wikipedia, news, books, Twitter and Reddit, so it ranks words by
   how people actually write — conversational and technical words included —
   rather than by how often they appear in print. A word in wordfreq's
   centibel bucket `b` (frequency `10^(-b/100)`) counts
   `10^11 × 10^(-b/100)`, the same order of magnitude as the Google Books
   counts below.
3. **Word pairs — Google Books Ngram, version 20200217**
   (<https://books.google.com/ngrams/>): all 47 2-gram shards of English
   Fiction (`eng-fiction`) and all 115 of French (`fre`) under
   `https://storage.googleapis.com/books/ngrams/books/20200217/`. Match counts
   are summed over letter case for publication years 1980–2019;
   part-of-speech-tagged entries are ignored. English uses the fiction subset
   because its dialogue is closer to what people type than the full corpus.

Every input is pinned by byte size and SHA-256 inside `generate.py`, and the
run aborts on any mismatch.

## Licences

The OSK is AGPL-3.0-only. The tables carry these terms:

* `words-en.txt`, `words-fr.txt` — **CC BY-SA 4.0 / GPL-3.0**. Their
  frequencies are an adaptation of wordfreq's data, which is licensed
  CC BY-SA 4.0 (<https://creativecommons.org/licenses/by-sa/4.0/>). CC BY-SA
  4.0 names GPLv3 as a compatible licence, so the adapted tables may be used
  under the GPL-3.0, and AGPL-3.0 section 13 permits combining GPL-3.0 work
  with the rest of Tilo. Changes made: filtered to the vocabulary above,
  truncated to the most frequent words and converted from centibel buckets
  to log-scaled weights. The French vocabulary also comes from the
  Grammalecte lexicon, see below.
* `bigrams-en.txt`, `bigrams-fr.txt` — Google Books Ngram data, Creative
  Commons Attribution 3.0 Unported
  (<https://creativecommons.org/licenses/by/3.0/>), filtered to the
  vocabulary of the matching `words-<lang>.txt`, re-aggregated over case and
  years, truncated and log-scaled.
* **SCOWL** — a permissive notice licence (Kevin Atkinson; its constituent
  lists are public domain or carry similar notice-only permissions, all
  reproduced in `LICENSE.SCOWL`). Sizes above 60 are not used, so the UK
  Advanced Cryptics Dictionary (size 80) is not included.
* **Grammalecte lexicon** — Mozilla Public License 2.0 (`LICENSE.Grammalecte`).
  It carries no "Incompatible With Secondary Licenses" notice, so MPL
  section 3.3 permits distributing the derived French vocabulary within a
  GPL-3.0 larger work; the source form is the pinned archive above.
* `extra-en.txt` and `generate.py` are part of Tilo (AGPL-3.0-only).

### Attribution

* **wordfreq** — Robyn Speer. (2022). *rspeer/wordfreq: v3.0* (v3.0.2).
  Zenodo. <https://doi.org/10.5281/zenodo.7199437>. wordfreq's data includes:
  * **SUBTLEX** word lists by Marc Brysbaert et al., freely available data
    (<http://crr.ugent.be/programs-data/subtitle-frequencies>), distributed in
    wordfreq with Marc Brysbaert's permission on condition that the SUBTLEX
    authors are credited: Brysbaert, M. & New, B. (2009), *Moving beyond
    Kucera and Francis* (SUBTLEX-US), Behavior Research Methods 41(4),
    977–990; van Heuven, W. J., Mandera, P., Keuleers, E., & Brysbaert, M.
    (2014), *SUBTLEX-UK*, The Quarterly Journal of Experimental Psychology
    67(6), 1176–1190.
  * **Google Books Ngram Viewer** (<http://books.google.com/ngrams>).
  * OPUS OpenSubtitles 2018, whose data originates from the OpenSubtitles
    project (<http://www.opensubtitles.org/>); Wikipedia; the Leeds Internet
    Corpus; ParaCrawl. The full notice is in `LICENSE.wordfreq`.
* **Google Books Ngram** (the bigram tables) — *Google Books Ngram Viewer,
  <http://books.google.com/ngrams>; Jean-Baptiste Michel\*, Yuan Kui Shen,
  Aviva Presser Aiden, Adrian Veres, Matthew K. Gray, The Google Books Team,
  Joseph P. Pickett, Dale Hoiberg, Dan Clancy, Peter Norvig, Jon Orwant,
  Steven Pinker, Martin A. Nowak, and Erez Lieberman Aiden\*. Quantitative
  Analysis of Culture Using Millions of Digitized Books. Science (2010).*
* **SCOWL** — Kevin Atkinson and contributors, see `LICENSE.SCOWL`.
* **Grammalecte** — Olivier R. and the Dicollecte contributors, see
  `LICENSE.Grammalecte`.

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

The run streams about 130 GB (nothing large is written to disk), needs only
`msgpack` beyond the Python standard library (uv installs it from the script
header) and rewrites all four tables in place. `--lang en|fr` limits it to
one language, `--words-only` rewrites just the word lists (about 70 MB of
download), `--jobs N` sets how many 2-gram shards are processed in parallel
(default 2, each a CPU-bound process) and `--out DIR` writes elsewhere. Same
inputs, byte-identical outputs.

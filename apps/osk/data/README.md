# OSK language data

Compact, embedded, **offline** language data for the Tilo on-screen keyboard's
swipe-to-type decoder and next-word predictor (workstream **W9**). All files
here are `include_str!`-bundled into the `osk` binary at build time, so the
keyboard works with no network and no model download.

## Files

| File          | What it is                                       | Size  |
|---------------|--------------------------------------------------|-------|
| `words.txt`   | `word<TAB>weight`, top-30 000 common English words, sorted by frequency desc. | ~376 KB |
| `bigrams.txt` | `w1<TAB>w2<TAB>weight`, top-40 000 word pairs whose **both** words are in `words.txt`. | ~630 KB |
| `generate.py` | Reproducible generator (run against the upstream Norvig n-gram files). | — |

`weight` is a compact log-scaled count: `round(log10(count) * 1000)`. It
preserves the relative frequency ordering in a small `u32`, which is all the
decoder/predictor need (they rank, they do not report absolute counts).

## Sources & license

Both files are **derived** from the
[Google Web Trillion Word Corpus](http://googleresearch.blogspot.com/2006/08/all-our-n-gram-are-belong-to-you.html)
(Brants & Franz, LDC), via Peter Norvig's freely-published n-gram subsets:

* `count_1w.txt` — <https://norvig.com/ngrams/count_1w.txt>
* `count_2w.txt` — <https://norvig.com/ngrams/count_2w.txt>

Norvig distributes his n-gram contributions under the **MIT license**
(see <https://norvig.com/ngrams/>). The underlying corpus counts are facts;
the same data backs the widely-used MIT-licensed
[`google-10000-english`](https://github.com/first20hours/google-10000-english)
word list. We bundle a cleaned, frequency-ranked **subset** (lowercase `[a-z]+`
only, length-capped, size-capped) suitable for offline embedded use in the
Tilo OS.

## Regenerating

```sh
curl -s -o /tmp/count_1w.txt https://norvig.com/ngrams/count_1w.txt
curl -s -o /tmp/count_2w.txt https://norvig.com/ngrams/count_2w.txt
python3 generate.py /tmp apps/osk/data
```

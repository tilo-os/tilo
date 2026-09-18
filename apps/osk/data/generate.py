#!/usr/bin/env python3
"""Generate compact embedded dictionary + bigram data for the Tilo OSK.

Sources (both derived from the Google Web Trillion Word Corpus, distributed
under MIT-spirit/educational terms via Peter Norvig's ngrams page):
  * count_1w.txt  -> unigram word frequencies
  * count_2w.txt  -> bigram frequencies

Output (committed into apps/osk/data/):
  * words.txt   "word<TAB>rank_freq"  one per line, sorted by frequency desc.
                rank_freq is a compact u32 weight (log-scaled count) so the
                embedded data stays small while preserving relative ordering.
  * bigrams.txt "w1<TAB>w2<TAB>weight" for bigrams whose BOTH words are in
                the dictionary, top-K by count.

We aggressively filter to lowercase a-z words (the swipe decoder + predictor
only handle the latin alphabet today) and cap sizes for a small, fast,
offline data set.
"""
import math
import re
import sys

SCRATCH = sys.argv[1]
OUT = sys.argv[2]

MAX_WORDS = 30000      # tens of thousands of common words
MIN_LEN = 1
MAX_LEN = 24
MAX_BIGRAMS = 40000

word_re = re.compile(r"^[a-z]+$")

# --- unigrams -------------------------------------------------------------
words = []           # (word, count)
seen = set()
with open(f"{SCRATCH}/count_1w.txt", encoding="utf-8", errors="ignore") as f:
    for line in f:
        parts = line.rstrip("\n").split("\t")
        if len(parts) != 2:
            continue
        w, c = parts[0].lower(), parts[1]
        if not word_re.match(w):
            continue
        if not (MIN_LEN <= len(w) <= MAX_LEN):
            continue
        if w in seen:
            continue
        try:
            count = int(c)
        except ValueError:
            continue
        seen.add(w)
        words.append((w, count))
        if len(words) >= MAX_WORDS:
            break

# Compact weight: log-scaled so a u32 holds the whole dynamic range and
# relative ordering is preserved. weight = round(log10(count) * 1000).
def weight_of(count):
    return max(1, round(math.log10(max(count, 1)) * 1000))

word_weight = {w: weight_of(c) for (w, c) in words}

with open(f"{OUT}/words.txt", "w", encoding="utf-8") as f:
    for (w, c) in words:
        f.write(f"{w}\t{word_weight[w]}\n")

print(f"words.txt: {len(words)} words")

# --- bigrams --------------------------------------------------------------
dictset = set(word_weight)
bigrams = []
with open(f"{SCRATCH}/count_2w.txt", encoding="utf-8", errors="ignore") as f:
    for line in f:
        parts = line.rstrip("\n").split("\t")
        if len(parts) != 2:
            continue
        pair, c = parts
        toks = pair.split(" ")
        if len(toks) != 2:
            continue
        w1, w2 = toks[0].lower(), toks[1].lower()
        if w1 not in dictset or w2 not in dictset:
            continue
        try:
            count = int(c)
        except ValueError:
            continue
        bigrams.append((w1, w2, count))

# Keep top-K bigrams by count.
bigrams.sort(key=lambda t: t[2], reverse=True)
bigrams = bigrams[:MAX_BIGRAMS]

with open(f"{OUT}/bigrams.txt", "w", encoding="utf-8") as f:
    for (w1, w2, c) in bigrams:
        f.write(f"{w1}\t{w2}\t{weight_of(c)}\n")

print(f"bigrams.txt: {len(bigrams)} bigrams")

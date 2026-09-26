# /// script
# requires-python = ">=3.11"
# dependencies = []
# ///
"""Regenerate the OSK's embedded English word and bigram tables.

    uv run apps/osk/data/generate.py

Inputs (pinned by URL, byte size and SHA-256; the run aborts on any drift):

* SCOWL 2020.12.07 (Kevin Atkinson, permissive notice licence, see
  LICENSE.SCOWL) decides *which* words exist: the English + American word and
  capitalised-word lists up to size 60, plain ASCII letters only.
* Google Books Ngram, English Fiction, version 20200217 (CC BY 3.0) decides
  *how common* each word and word pair is: match counts summed across letter
  case over the publication years YEAR_FROM..YEAR_TO.

Outputs, written next to this script:

* words.txt   ``word<TAB>weight``, the MAX_WORDS most frequent SCOWL words.
* bigrams.txt ``w1<TAB>w2<TAB>weight``, the MAX_BIGRAMS most frequent pairs
  whose words are both in words.txt.

``weight = round(log10(count) * 1000)``. Ties sort alphabetically, so the same
inputs always produce byte-identical files. Everything is streamed; nothing
large touches the disk. Expect ~26 GB of download.
"""

from __future__ import annotations

import argparse
import gzip
import hashlib
import io
import math
import re
import sys
import tarfile
import urllib.request
from collections import Counter
from concurrent.futures import ProcessPoolExecutor
from pathlib import Path

HERE = Path(__file__).resolve().parent

MAX_WORDS = 30_000
MAX_BIGRAMS = 40_000
MAX_LEN = 24
YEAR_FROM = 1980
YEAR_TO = 2019
SCOWL_MAX_SIZE = 60
SCOWL_CATEGORIES = (
    "english-words",
    "american-words",
    "english-upper",
    "american-upper",
)
# Pairs rarer than this within a single shard are dropped before merging to
# keep memory bounded. The rarest kept bigram is counted about 200 000 times,
# so the few case variants this can shave off never change the selection.
MIN_SHARD_PAIR_COUNT = 1000

SCOWL = (
    "https://downloads.sourceforge.net/wordlist/scowl-2020.12.07.tar.gz",
    2569810,
    "5587667caa20c4891390c2d42dbb4d5c4c3f41bee77af1457ece3ba23fb859cc",
)
NGRAM_BASE = "https://storage.googleapis.com/books/ngrams/books/20200217/eng-fiction"
UNIGRAMS = (
    f"{NGRAM_BASE}/1-00000-of-00001.gz",
    940045192,
    "8640fb65e15484592665237df129de132032e25354d5f984f61c32100d901461",
)
BIGRAM_SHARDS = 47
# shard index -> (byte size, SHA-256)
BIGRAM_LOCK: dict[int, tuple[int, str]] = {
    0: (543460529, "f047f6dc7ecbb47cefd3f24a18def52904553dd98900900c9e5f473b71545788"),
    1: (795591580, "d1fc1ee9149d7424a6d5c68e2b07d47d5bbe91c431ec05cd0e4611c1692b524c"),
    2: (595353082, "a4bd0b89c78636190946a1eaecde91ad7f415aa75b3f8d91f613b25a250c8e09"),
    3: (571615512, "e44c7c100b7a79aeadcd4cb08c5165ea127732612c72aa39b52c11894d742079"),
    4: (570434025, "c4bd3ef586717e17f9c63c049bc0da275469f68bf86bc9fe1afa400e26acc225"),
    5: (557154172, "247ba89092e4af354a4ce3ab907202e1f192d090bbb117f67d070ed36352c393"),
    6: (574023106, "47012d45c03b13ebae11ad026e10d4c40635fea193bae1e8724b5795727ca4b6"),
    7: (610893580, "bc8d1cbb3af629e3d51c052c237952cafe28d8d87f199d3c1a2c7a44d4e2410b"),
    8: (544737302, "c3ec99056a251761d6e7d73c5caa45bea13d96ecc63c9e6f1102a1f95736a521"),
    9: (521105879, "b3ee92a6d67c971b7a986324153d58905e9d9228c085bf143882995c52fc9603"),
    10: (583761728, "952e1f8838a95a76bac370f0a950ae77eba7507322518d01fb468d660edbbead"),
    11: (563588679, "5664bd583b210e6f4b0e674e75fd403b9f8316aa3d67ce9a5da998aabb68ded6"),
    12: (559594780, "1d510ac53093f804258624dec43b29536134581d982d93d70086d131d28f20e8"),
    13: (637057019, "fa4bc884b67465eabdcd16dcbc89a1c1e09af9d3d5bf8456b20aa29448d2cfa8"),
    14: (652897088, "c9ef58117ad461664f9a106b0aad526e1904ecdd6bbf1932192fb18f6b7fc9db"),
    15: (
        1009475786,
        "435a27c6bd4a5761b58161161ab584c147668c5c80ff7879dca8471867b99633",
    ),
    16: (899355136, "9a79bcacf4b8d5f8dfe4408316e5ace64bc7d36ae6bf40c1c6fb41d768f116db"),
    17: (759490628, "77db635fc541d5632ada03c580d7d0dd7423ffc3590c3ba864159ae04c5331c7"),
    18: (774724313, "8776f7a722c215d4c22101b92ba4d73aba4e3763d631b601463b09c69787b38a"),
    19: (672832697, "21f2e4898e6a9c24bda84db8fc197346854c77deee30b44f15047fb18a907a09"),
    20: (629684561, "e4cc59c4cdda1e7fed004e6aeadb414cd14c33334fd19d67342b7904ab0b3b3a"),
    21: (654774783, "fb6c5762f69b3d31827ca97d5180e91055816dcbefe8515e87dbf81a2fd9a4de"),
    22: (655799866, "5af53067243afa7411a3f669b59888f994bf481eb3bb0e3485ed033fdd52e628"),
    23: (641671712, "15ec7acc75c7d2a39fe7f193ce965849deda278b107cc667605bef6800f02c25"),
    24: (653429276, "e19f30bdb5172088ad84fb6d4311b978f1fd02a9d266243a77d5d1fac9652ce7"),
    25: (667326597, "53038c99d8dd67d17af146b2cd2e490c94f0c0a539022478670a97e77799a97a"),
    26: (656117594, "8046db00e5c1bee7ce34e34e6266efcda975e1ea0874fc57dfd1b26944bd9eec"),
    27: (645988872, "7dec16cc1bfdf021fe4ed177065bf5fbff7495112b420d7920ed8e6ec3fd83c9"),
    28: (658605068, "24607f5260659101bb236354b97808d0840a4a1e6fc5a22aee5e3e2c1651aa60"),
    29: (717012819, "545bd3de042a1ce4d031446d0332acc91af28bee520cd74a0928073e836ec6cf"),
    30: (687823665, "b2b509a0f69ccfc0c24fdfa26a5f53e8ee862e2c3485a586b32e6718ead9ef50"),
    31: (651525099, "1407c2785a6fdef5d5ea575179071bad267cdde87fba93764abea654cbdc4ea5"),
    32: (656563741, "53e580584b5c4eb447b9d6fba52af1bf08d6488b73619db290b365bed35a2ce4"),
    33: (662372092, "37510f31dde666d9ffe30abab33265d0767101256af3a025f4a6362a9941ed31"),
    34: (778333677, "1319a56dedcf9d1ba17446bdc7fda0b43d407d12ab961cec1f586b86d822102f"),
    35: (710639469, "db2e849cdba881ee6cd4cb1afbd74ad17df0dd7b5bbca9a8ff6a3421503b89db"),
    36: (637070475, "e7f68eee9e40ff44650a0a65a77fb04aca7c4769b34eabf5c2ef43a9b62f1b8d"),
    37: (629523068, "911055dbdf4e3db925783f4ce3f946a9b8d4a90b6fd26bfdf39b140d4f67103f"),
    38: (656503970, "c91c5a64a75918e110014305d14deea0ec31e2af52898102c370066679edfbfa"),
    39: (647666576, "da90efebb28c84c3686204115bf2bbb12a30b3466bb54badb1e919d43ecd04cc"),
    40: (650298767, "0e988b20a963429d2f30342ac6059d7405a746a869e25dea3a63aa06800a7fae"),
    41: (648020282, "dd81afd69caf1eb3c85bece433da96dfd89008d1d80e5950cb55d4cc255dcb03"),
    42: (806202417, "5b86fc661f78714e29d8b1fbee7a111d0b4e2855137a08888d2a26af5f64fba9"),
    43: (768288456, "addb238ade0152a1146b8a20fae2f4fb50f3f00348aafca6fad35b3fe23d691f"),
    44: (627430143, "4750016f3debe0a39395b54a32602bbaf44c8fab3b354c36efe8900657674e54"),
    45: (703689048, "aab81b4d8bab4bfc8091ccc3c8cafb07af49e14209230f8a64dccf0c86e20635"),
    46: (849832502, "32a9f1e278ef5aa8a6ca310c522ad66e3806fb1b0694b1927822e88bb8057074"),
}

WORD_RE = re.compile(r"^[A-Za-z]+$")


def open_verified(url: str, size: int, sha256: str) -> "VerifiedStream":
    return VerifiedStream(url, size, sha256)


class VerifiedStream(io.RawIOBase):
    """A streaming HTTP body that hashes itself and checks the pin at EOF."""

    def __init__(self, url: str, size: int, sha256: str) -> None:
        self.url, self.size, self.sha256 = url, size, sha256
        self.resp = urllib.request.urlopen(url, timeout=120)  # noqa: S310 - pinned https URLs
        self.digest = hashlib.sha256()
        self.read_bytes = 0
        self.verified = False

    def readable(self) -> bool:
        return True

    def readinto(self, buf) -> int:  # type: ignore[override]
        n = self.resp.readinto(buf)
        if n:
            self.digest.update(memoryview(buf)[:n])
            self.read_bytes += n
        else:
            self.verify()
        return n

    def verify(self) -> None:
        if self.verified:
            return
        got = self.digest.hexdigest()
        if self.read_bytes != self.size or got != self.sha256:
            raise SystemExit(
                f"{self.url}: expected {self.size} bytes sha256 {self.sha256}, "
                f"got {self.read_bytes} bytes sha256 {got}"
            )
        self.verified = True


def gz_lines(url: str, size: int, sha256: str):
    raw = io.BufferedReader(open_verified(url, size, sha256), buffer_size=1 << 20)
    with gzip.open(raw, "rt", encoding="utf-8", errors="replace") as text:
        yield from text
    # Drain so a truncated gzip trailer can't skip verification.
    while raw.read(1 << 20):
        pass


def year_total(fields: list[str]) -> int:
    total = 0
    for f in fields:
        year, count, _volumes = f.split(",")
        if YEAR_FROM <= int(year) <= YEAR_TO:
            total += int(count)
    return total


def scowl_words() -> set[str]:
    url, size, sha = SCOWL
    words: set[str] = set()
    raw = io.BufferedReader(open_verified(url, size, sha), buffer_size=1 << 20)
    with tarfile.open(fileobj=raw, mode="r|gz") as tar:
        for member in tar:
            name = member.name.split("/")
            if len(name) != 3 or name[1] != "final":
                continue
            category, _, level = name[2].rpartition(".")
            if category not in SCOWL_CATEGORIES or int(level) > SCOWL_MAX_SIZE:
                continue
            data = tar.extractfile(member)
            assert data is not None
            for line in data.read().decode("latin-1").splitlines():
                w = line.strip()
                if WORD_RE.match(w) and len(w) <= MAX_LEN:
                    words.add(w.lower())
    while raw.read(1 << 20):
        pass
    return words


def unigram_counts(vocab: set[str]) -> Counter[str]:
    url, size, sha = UNIGRAMS
    counts: Counter[str] = Counter()
    for line in gz_lines(url, size, sha):
        head, _, rest = line.partition("\t")
        w = head.lower()
        if w not in vocab:  # also rejects POS-tagged forms like "the_DET"
            continue
        n = year_total(rest.rstrip("\n").split("\t"))
        if n:
            counts[w] += n
    return counts


def bigram_shard(args: tuple[int, frozenset[str]]) -> tuple[int, Counter]:
    index, vocab = args
    url = f"{NGRAM_BASE}/2-{index:05d}-of-{BIGRAM_SHARDS:05d}.gz"
    size, sha = BIGRAM_LOCK[index]
    raw = io.BufferedReader(open_verified(url, size, sha), buffer_size=1 << 20)
    counts: Counter[tuple[str, str]] = Counter()
    with gzip.open(raw, "rt", encoding="utf-8", errors="replace") as text:
        for line in text:
            head, _, rest = line.partition("\t")
            w1, sep, w2 = head.lower().partition(" ")
            if not sep or w1 not in vocab or w2 not in vocab:
                continue
            n = year_total(rest.rstrip("\n").split("\t"))
            if n:
                counts[(w1, w2)] += n
    while raw.read(1 << 20):
        pass
    kept = Counter({k: v for k, v in counts.items() if v >= MIN_SHARD_PAIR_COUNT})
    return index, kept


def weight_of(count: int) -> int:
    return max(1, round(math.log10(max(count, 1)) * 1000))


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--out", type=Path, default=HERE)
    parser.add_argument(
        "--jobs", type=int, default=2, help="parallel bigram shard downloads"
    )
    args = parser.parse_args()

    vocab = scowl_words()
    print(f"SCOWL candidates: {len(vocab)}", file=sys.stderr)

    counts = unigram_counts(vocab)
    ranked = sorted(counts.items(), key=lambda kv: (-weight_of(kv[1]), kv[0]))[
        :MAX_WORDS
    ]
    (args.out / "words.txt").write_text(
        "".join(f"{w}\t{weight_of(c)}\n" for w, c in ranked)
    )
    print(f"words.txt: {len(ranked)} words", file=sys.stderr)

    dictionary = frozenset(w for w, _ in ranked)
    pairs: Counter[tuple[str, str]] = Counter()
    with ProcessPoolExecutor(max_workers=args.jobs) as pool:
        work = [(i, dictionary) for i in range(BIGRAM_SHARDS)]
        for index, shard in pool.map(bigram_shard, work):
            print(f"  2-gram shard {index}: {len(shard)} pairs", file=sys.stderr)
            pairs.update(shard)
    top = sorted(pairs.items(), key=lambda kv: (-kv[1], kv[0]))[:MAX_BIGRAMS]
    top.sort(key=lambda kv: (-weight_of(kv[1]), kv[0]))
    (args.out / "bigrams.txt").write_text(
        "".join(f"{a}\t{b}\t{weight_of(c)}\n" for (a, b), c in top)
    )
    floor = min((c for _, c in top), default=0)
    print(f"bigrams.txt: {len(top)} bigrams, rarest counted {floor}", file=sys.stderr)


if __name__ == "__main__":
    main()

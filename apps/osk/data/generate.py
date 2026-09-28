# /// script
# requires-python = ">=3.11"
# dependencies = ["msgpack==1.1.0"]
# ///
"""Regenerate the OSK's embedded word and bigram tables.

    uv run apps/osk/data/generate.py [--lang en|fr ...] [--words-only]

Per language, three pinned inputs (URL, byte size and SHA-256; the run aborts
on any drift) decide the tables:

* Membership, *which* words exist:
  - en: SCOWL 2020.12.07 (Kevin Atkinson, permissive notice licence, see
    LICENSE.SCOWL), the English + American word and capitalised-word lists up
    to size 60, plain ASCII letters only, plus the everyday and technical
    words in extra-en.txt that SCOWL lacks.
  - fr: the Grammalecte 7.7 French lexicon of inflected forms (Olivier R.,
    Dicollecte, MPL-2.0, see LICENSE.Grammalecte), without proper names,
    letters a-z plus the French accented letters only.
* Unigram frequency, *how common* each word is: wordfreq 3.1.1 ``large_<lang>``
  (Robyn Speer, data CC BY-SA 4.0, see LICENSE.wordfreq), which blends
  subtitles, web text, Wikipedia, news, books and social media, so it ranks
  conversational words the way people type them.
* Bigram frequency, *which word follows which*: Google Books Ngram version
  20200217 (CC BY 3.0), English Fiction or French, match counts summed across
  letter case over the publication years YEAR_FROM..YEAR_TO.

Outputs, written next to this script:

* words-<lang>.txt   ``word<TAB>weight``, the most frequent member words.
* bigrams-<lang>.txt ``w1<TAB>w2<TAB>weight``, the most frequent pairs whose
  words are both in words-<lang>.txt.

``weight = round(log10(count) * 1000)``. wordfreq stores each word in a
centibel bucket (frequency ``10 ** (-bucket / 100)``); its count is that
frequency per WORDFREQ_SCALE words, the same order of magnitude as the Google
Books counts, so weights of either source mean the same thing. Ties sort
alphabetically, so the same inputs always produce byte-identical files.
Everything is streamed; nothing large touches the disk. Expect ~130 GB of
download for both languages, or 70 MB with ``--words-only``.
"""

from __future__ import annotations

import argparse
import functools
import gzip
import hashlib
import io
import math
import re
import sys
import tarfile
import urllib.request
import zipfile
from collections import Counter
from collections.abc import Callable
from concurrent.futures import ProcessPoolExecutor
from dataclasses import dataclass
from pathlib import Path

import msgpack

HERE = Path(__file__).resolve().parent

MAX_LEN = 24
YEAR_FROM = 1980
YEAR_TO = 2019
# wordfreq frequencies are scaled to counts per this many words.
WORDFREQ_SCALE = 10**11
# Pairs rarer than this within a single shard are dropped before merging to
# keep memory bounded. The rarest kept bigram is counted well over 100 000
# times, so the few case variants this can shave off never change the
# selection.
MIN_SHARD_PAIR_COUNT = 1000

WORDFREQ = (
    "https://files.pythonhosted.org/packages/24/61/"
    "62835c475d69872d30689f284497853fe33fe1d6dd18f57346d13305861d/"
    "wordfreq-3.1.1-py3-none-any.whl",
    56834549,
    "4b1c6ecffc6198be3396d5cf871c4423ca71c907c231348d352dd54d62b97473",
)

SCOWL = (
    "https://downloads.sourceforge.net/wordlist/scowl-2020.12.07.tar.gz",
    2569810,
    "5587667caa20c4891390c2d42dbb4d5c4c3f41bee77af1457ece3ba23fb859cc",
)
SCOWL_MAX_SIZE = 60
SCOWL_CATEGORIES = (
    "english-words",
    "american-words",
    "english-upper",
    "american-upper",
)
EXTRA_EN = HERE / "extra-en.txt"

GRAMMALECTE = (
    "https://www.grammalecte.net/dic/lexique-grammalecte-fr-v7.7.zip",
    12325782,
    "bdccf2065e252010253171000a38de84581407896b6bc60aca7d4d69dab7e4d2",
)
GRAMMALECTE_MEMBER = "lexique-grammalecte-fr-v7.7.txt"
# Lexicon tags of proper names (places, people, organisations, first names).
PROPER_NAME_TAGS = frozenset({"npr", "prn"})

NGRAM_ROOT = "https://storage.googleapis.com/books/ngrams/books/20200217"

EN_WORD_RE = re.compile(r"^[a-z]+$")
FR_WORD_RE = re.compile(r"^[a-zàâäçéèêëîïôöùûüÿæœ]+$")

MAX_WORDS_EN = 40_000
MAX_WORDS_FR = 50_000

# 2-gram shard index -> (byte size, SHA-256)
BIGRAM_LOCK_EN: dict[int, tuple[int, str]] = {
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
BIGRAM_LOCK_FR: dict[int, tuple[int, str]] = {
    0: (651353854, "0205641ce4a98d34eb1ff72fb6e097897ba412849a71bccbb6eb3cf38e66246a"),
    1: (776977144, "2ccf48ab42931b31aa5e70b37d268fc48eec561e625ce9d91bd34f6d8bf85022"),
    2: (949064595, "846abd6863bc3173604f73b22af4ea4845dcb42bf9ef40f478261f543d019650"),
    3: (990024701, "885939ca2c18826599b12485e1efb62500cff6ef6d994a9e8fce6e68d4ace9dd"),
    4: (843947690, "f01aa6d4fdf41737a1641e2e6e9e9be701dbbfa8e5af3639bf4e01a4d8789aef"),
    5: (718778153, "4a0853f30795692bacc05ec50b8d61400aa175d8329afe65ecbba23d8013ad06"),
    6: (701354190, "9e87c8f9ad72f9c45e8524bb2cec79632057dd6c23aedb215ad970563abf3d35"),
    7: (673808251, "4e263b9e23ee3c197bd91c98d8c563d1fb5ca9777ab5fa08efe246f63a1a4101"),
    8: (695633731, "b7998b41b424d90ff662fcf247fc52df82f7c9d197914af174e6966fba6606dc"),
    9: (769707208, "6d4ca3eea899400a60d5209647d2c0d2b6728256612c99f2b53fe2959582199d"),
    10: (674881247, "c00956cbe189be8a0b4a43c202e1a010d799fb2c4ffc7715ac3c49db385a8fbf"),
    11: (754215445, "081d137c9b78f7eaf6389021d5cff7f6f11a130cbb93239acd4aa909180b07f3"),
    12: (707725546, "c727a35324ec6e16e62fce28a98ad50f2d6a6fbe9327e0dd35bf24c0fe30c6bb"),
    13: (719512060, "14761ed4bef5b4ac53e21fbc4999a78a66f8b0eada81147930061d38b38e5079"),
    14: (801790492, "5a400e088fbef5301a4f53f617513acb3752f00a7410b0be2905fb06bb651b8f"),
    15: (764619062, "b7694a7c5b2c70e5c46a8a3bb20da0be9f173b801716ab0b4bd66fa826c8729a"),
    16: (755920239, "bc81a2a74db13ef02637c9e5cf64ff95fc2acaf2ebdc0fd1f425385c492c2b53"),
    17: (776767456, "1b1b1214d87b780ef6cbb5e218b80658695ceb6f294cce00e25280ecb47c71da"),
    18: (732791232, "c22d9396c2e2478a74f40ea4e1ade6183a9c0826f4529b48f525184207a7bab9"),
    19: (710721144, "c7b00e838d66e6e7c3ee2be8d66783f74c6dd57538832c204b7c23b807872151"),
    20: (724286513, "0b09f8a2787e7e1f95a58d6d883b119de6769a51732dfb390816650bb0ad378b"),
    21: (736969533, "d5d968052f11fdc0b3ef605c0bab9ef62321d71dcf2297f4241c16867ce14796"),
    22: (757478811, "c94c6b3d3fca169c9153ddb95e52667cf009195a53da7e74b4487208419bed64"),
    23: (793814857, "c5e21002b8a7e11ab4bcbc7a469a0c069b6fa2582561991d50f0be72de9751ed"),
    24: (723500986, "8d301d800cbb9aca12c85d971ba343ab2c9f10467ffb388d9e41284aba5e249d"),
    25: (724392540, "debe101beb612fe191a2dfdd56fbfce4b5c6114945908718a0d57d57fa0a82e4"),
    26: (750857541, "471571c0854cef1266d339d146ff399305e1781716b4377dd847a20cd492709d"),
    27: (757356824, "1c19ec19286c77195fc89fec4088c89126f4c8d0b8dfc0f4a6af3decf468d103"),
    28: (776172062, "fb6add9634bd84d5859c59f74b17cf1114ed043b4760dbd4e75ff2688512fdf5"),
    29: (735359071, "67cc6427b55ddb32f6642057ed6bb8a145f5047cc83186e87b36f3f967f5f704"),
    30: (750982566, "95c9a3fd6a8c537ab7be09b038e15cfbae76ece39546115c4670df3901494077"),
    31: (664737127, "0ad7f487b38c8db6c61865f9d9d407033075529f3a15823f24d817e0f5508adb"),
    32: (740628639, "554192a6712d82f45046c64b7ed072aa28941b93c9b8e87d2f36db4788ca9d29"),
    33: (741774308, "63bf06a1bd9b72ed6c42a4af3f7abd85449578d1acea926c2d701c2a46d06e80"),
    34: (778280357, "7458c38a099b8205e15d6a961245990b20b5f8e972bb3cc255b1f09813e4fa55"),
    35: (
        1017647500,
        "6f83104bf4285923a53ccc5301dd15684097af43c91c2cc93453c169d6e3bcdb",
    ),
    36: (993623419, "a0d4d31083468ca2345d7599e659bf7f0bca3d62bfdae79b3d1815837852de24"),
    37: (
        1073459750,
        "ec5c1e3068beef33301d4bc41a6fee2ce960da6de46985a1f9cb76609e9d40e7",
    ),
    38: (997853246, "3bd71fb05556e99c82ada92f7330f8b505f270fc81ad511825a40ab8dbc0407d"),
    39: (929251660, "0f172b787d849999b6c7102a7504ca341d2f78f018aeee7195ebf40076dcbee0"),
    40: (
        1015534441,
        "1803faa7bb9d822a5cf9b4c52d1bf8a317be0b5f2bc7b4113dc1d93f116edcf4",
    ),
    41: (962364393, "d794778b4d732e2a19a639187524e6ba20820f0c970a28dd6c7e1a9f7b32588f"),
    42: (
        1037318679,
        "6712e341d0c351b3413fe6360fe2cf2b1cde4ff3ba7f4005ac99c280adce09ab",
    ),
    43: (908345103, "fd98c3911be29074d0b0d7497f604bb8097e23f605d3e73138da15e9d242b816"),
    44: (874034405, "78d6b0192c3bd1cc12df01f2e703cab7cd1c5a5ffa044617c33935acad14df20"),
    45: (876988251, "6418611103f7ccc07522986f240b6de7d9a805f2a8e6a682517bb07cea8b1a1b"),
    46: (835940648, "e4d11c3d47fbead45c62e9e23045bd5fac05d15ac9893a9b1ba3b4527d640a74"),
    47: (876831934, "c0e4168ebd2b3b522a358e499a4ed13dd206d21b0210eb4a719c60c7136b027d"),
    48: (957131163, "301b1c5589ec69f9172f552970655bfc0f458df47abbbf4244f1856fa3fd8992"),
    49: (915250597, "f22268ea9208ae9197d013aa7258ee2648cd077bc2cdd37a812e3949293f0266"),
    50: (827907101, "2cb75de3b05e9ad063e714183dbf9dda029a249e9521ed4088bf8a3f4821e9a4"),
    51: (850564921, "2569c874d6cff11738e14e14207351e4b42d81925d9a376ad49c4fb9a5621d4a"),
    52: (888599078, "584f3e087635f43d64f5775a5151f9e464e2a5fe3ebcb1cfbc526a98b8f33ef1"),
    53: (815743530, "18cc66110a887bb5f0c970594274ad10f3f35e8da5c88292e4531bfac9be1250"),
    54: (865299347, "597a4f8963450db529b8f343208c5b2a64fcbc912032d74d3879f2191410010e"),
    55: (909068155, "e84cfdce4429953245d4e2ec7056ff2b7b3f605719306e28463217ada8bd362c"),
    56: (882356097, "01090920819c4d07d989c8660251eec0dcbb89b6c40e758207a41eeb1ac3965e"),
    57: (892574986, "41ae50a880c52511f83ba92819877450c871fdf8d79698abcf07fbf3497a2c82"),
    58: (921053014, "2e9c0f37d58156c01d358d183c208fb090f0c077b7c55e75710496f8e5fbee98"),
    59: (918582219, "8b2cc6174458a31ac6c362f96730b9a98608e3d44c0e9a25367b036a573be34a"),
    60: (940383391, "f956caf8913087380a988a16be92aad4dd6aca7f0b017d54828e18f16f64e7ac"),
    61: (890696859, "8f456f8b6fe58a650f6536e960582d75878851a52e969faf0dad3cc2466818b8"),
    62: (881321751, "f4ffc63d5b344f59237bda298825dd728125f02ad9b6e3e044f68984426b7bce"),
    63: (896020952, "3c964dde3ec600204294b2fc79021cdccc326c10be495ea45e91bebd6fb5adfc"),
    64: (923824267, "0b5af7fa8b129a68b5f055f4de80f08d0d5566018f5f7798366551b01fa2ef18"),
    65: (910964657, "74bcba2e4cee63d6f9f39acb014c00522104913c7a32e56e6b1ddb00d1564145"),
    66: (907654959, "6f5eee300cddb0759bb2c8dcabf592d01eb923d027516a1c9cb0ddc7e6c62ad5"),
    67: (923053784, "3f6ee7778b106970fb1603c2320d03d4e4b0497a6b97ca325889a4b31bc69904"),
    68: (932286124, "41c474dca8d99fc37d70bb87b02843a27200acef9cc311b8b9e3f5fe09dfcdad"),
    69: (886443302, "2a3fadb0e6a51cfbe27f2438d304bf43041752b3f5b085fe1ff87ccde4396314"),
    70: (886679304, "054cd82464feb366d24fb5fa3cc67ab9474d90c1ea0e050d902b58c6f7600dc4"),
    71: (832919169, "d0423851aad188484d1a301ff9b82ad4b4909b631137b82adf97d12bc86f9ceb"),
    72: (833411521, "ab6c9f0497b448a86fd8bcacfb1f3afde5b478493b7191773140a336d878e2e1"),
    73: (816544130, "06c26e3296a980ddcbc6fdd6c690fd0d038cf0e4347ba00cfde005d3ff5398d9"),
    74: (843397294, "52a2005d58569c77522507ca8abd6a3f1cf7cf05eab7b01e72f83b665fe8cc10"),
    75: (825353440, "0bf6ceea6f22b5cbd1813e55239f18c76c9fcf2b645615dfa5fd7f979bff1c5b"),
    76: (890504550, "04509bbf97658568619765f3433ad35f57f42e4f262a1460659aafdac83b9dec"),
    77: (979788239, "e3158240193af79944ce42e1bea643de87e5ae072350fe4ee2c2a74920fff43b"),
    78: (
        1013861589,
        "f6005a6a75f6d98f99a41081b2e94131c75fa548d23290ca2dc5de7efe541ec3",
    ),
    79: (936319624, "da439fbd4e5305062a3c233c72793ad238cfc075698d793a594f4c917b1f9d05"),
    80: (852242830, "686bd4b3ef1f99844e6d06cac3732d158e7fd2c562544900a80c34e12bf1a904"),
    81: (868617886, "14a73f9fc86f6f5a2df969a362a8348d383f0fa7297ec0d01f31fc075074eb2b"),
    82: (874659336, "46e7864d04f1f851b86255daf67f1f77556960359b4a1c2827486dbe8bd5dfd7"),
    83: (797459711, "a86b87c7150750d023e182694cfaf498f75798fbf5bcfb1452b63163dd0b5d4e"),
    84: (896799690, "0e023c8538e8dab4712fba307f8dcb3ec2c7ba0548b1aebe7fe212a22e68df80"),
    85: (890849462, "4b97825e0974dc2bbf0c8f633aefca7df6c876d37c77839188c806891fe206a6"),
    86: (927524237, "9bbb85c08b0d379574200854ef493fc743dd7188033b4850b264ea875e82a067"),
    87: (909505078, "94efc6dd40ef4c0dfeedef91a66b7b1f73c2c1d08a72902110e7069fec731d3d"),
    88: (850420700, "c0dbc0ca7714c955368d0771cbdd170ddb6b58d950bb17af1d06e26b342ce67c"),
    89: (931463261, "2994817dd52ee48d9990338dc3f36ff3ebcfee6dba70282f925293b84913f632"),
    90: (910655334, "c52c658969812733d4ca9859084adfc3ead57e250bb4be0eef295c2de34197e6"),
    91: (900036634, "7204e7a026098578972011334b852cdedcb7f386f7e99307100203d5534ccb8c"),
    92: (951812081, "df1d5aa4df395d6c77d7edade720ed33a3c52ed37ca3371907ce69d657afe10a"),
    93: (896188391, "4b1d270b5c47acc6d754ad9f2e3edc36d26b8917901e498785b1021b9bb03330"),
    94: (787755482, "96a0f68c91f86415078014de71c4156f75ddd264b9d23118ec30fccad0226aa0"),
    95: (999837641, "324cfa2952e33d9f238f652e4e56888d9815cab79b590bccad64e54d1cfa4bce"),
    96: (947809805, "1259c5996330d1857ccb262d0990c5b8b3c374dfdb33a66da04aaabc92e006cf"),
    97: (905830629, "5a07e00696d9c5d6407a3386477bf895e7923bd9722b25bb2c3ecf9ec0935043"),
    98: (861466451, "d897f2589aff7bfaacbe77c362bc90124e6aa2fb46cf36b1aaf681699363fa41"),
    99: (871840588, "a6850374a3e64da61ef82618c6c6cffcfdad2c05fa0fb98904762099d01462a0"),
    100: (
        939649059,
        "4bf713fbbac53336960cd18b6e755dcecbad40d4971a6cdce09d5ba5d87314de",
    ),
    101: (
        921824989,
        "a95332213c44ce0bfa4f49282e193ad5cd9fdd0e497acbabe10d4591c3e201dd",
    ),
    102: (
        901559436,
        "dac03ec3551f5ff1ee0513db5e2424979625c526b618b98c8c55bfce2181ec53",
    ),
    103: (
        932224121,
        "8243d2224e6b0a4b202e3a7e15df1428a8866c7b19e1d1bb6b64d68aeeec7c2e",
    ),
    104: (
        758559352,
        "3941e4026724acd14cc0e502b6a5b6633f0cfea862bd861fd9845d8241f9fc1c",
    ),
    105: (
        819910402,
        "db540556a386bb7b1c61e3bcc737dae80ec4dbe254e6bcc42380823757eb511d",
    ),
    106: (
        874056603,
        "c8aeb6622e0b218303dc0ed99dbd78c93d24505e572ef18a8bd3b869eb70733a",
    ),
    107: (
        928521365,
        "c2063782ac6c4c134f8a42d4f56023d756d39958b2a365fe8ac18d0ac2945556",
    ),
    108: (
        926737884,
        "2f05b7a79eb329969dc7197c7c945870738ad372a58f2e3b6309eb4add7eb0c0",
    ),
    109: (
        855568575,
        "8ae163d76258250c01f25045388e2b38e91bf316647ecdaef65b2033bf91f3d3",
    ),
    110: (
        912869358,
        "24e2fbff804faf1ddb7681686ddc4b10e282836032bda851234f2d1ca995cb97",
    ),
    111: (
        905782855,
        "6839c04e0e673bd6d1297beb72fa5ebc19b45c8e9820a4ffbc001ab045a2d1a2",
    ),
    112: (
        870949894,
        "9a6554664df7f02961fe0a2716fe9b537549155aa73a23aca21d4ddf4fe11019",
    ),
    113: (
        892180415,
        "a2ba845a4bc3a7cec32bfbc6ffb17608b6b68b1b5a01ffa88771285ba3b2e470",
    ),
    114: (
        1177472944,
        "6fee9605cb8a7298d36fe6281392d50a5f715b12c8215284cdf521b377e756e0",
    ),
}


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


def verified_bytes(pin: tuple[str, int, str]) -> bytes:
    """A small pinned download, read whole (zip archives need to seek)."""
    stream = open_verified(*pin)
    body = stream.readall()
    stream.verify()
    return body


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
                w = line.strip().lower()
                if EN_WORD_RE.match(w) and len(w) <= MAX_LEN:
                    words.add(w)
    while raw.read(1 << 20):
        pass
    return words


def extra_en_words() -> set[str]:
    words: set[str] = set()
    for line in EXTRA_EN.read_text(encoding="utf-8").splitlines():
        w = line.partition("#")[0].strip()
        if not w:
            continue
        if not EN_WORD_RE.match(w) or len(w) > MAX_LEN:
            raise SystemExit(f"{EXTRA_EN.name}: {w!r} is not a plain lower-case word")
        words.add(w)
    return words


def english_vocabulary() -> set[str]:
    scowl = scowl_words()
    extra = extra_en_words()
    print(f"en: SCOWL {len(scowl)} words, {len(extra - scowl)} extra", file=sys.stderr)
    return scowl | extra


def french_vocabulary() -> set[str]:
    archive = zipfile.ZipFile(io.BytesIO(verified_bytes(GRAMMALECTE)))
    words: set[str] = set()
    columns: list[str] | None = None
    with archive.open(GRAMMALECTE_MEMBER) as raw:
        for line in io.TextIOWrapper(raw, encoding="utf-8"):
            fields = line.rstrip("\n").split("\t")
            if columns is None:
                if fields[0] == "id":
                    columns = fields
                continue
            row = dict(zip(columns, fields))
            if PROPER_NAME_TAGS.intersection(row["Étiquettes"].split()):
                continue
            w = row["Flexion"].lower()
            if FR_WORD_RE.match(w) and len(w) <= MAX_LEN:
                words.add(w)
    if columns is None:
        raise SystemExit(f"{GRAMMALECTE_MEMBER}: no header row")
    print(f"fr: Grammalecte {len(words)} words", file=sys.stderr)
    return words


@functools.cache
def wordfreq_wheel() -> zipfile.ZipFile:
    return zipfile.ZipFile(io.BytesIO(verified_bytes(WORDFREQ)))


def wordfreq_centibels(code: str) -> dict[str, int]:
    """Each word of wordfreq's ``large_<code>`` list -> its centibel bucket
    (0 = every word, 100 = one in ten, ...)."""
    packed = wordfreq_wheel().read(f"wordfreq/data/large_{code}.msgpack.gz")
    header, *buckets = msgpack.unpackb(gzip.decompress(packed), raw=False)
    if header != {"format": "cB", "version": 1}:
        raise SystemExit(f"large_{code}: unexpected wordfreq header {header!r}")
    centibels: dict[str, int] = {}
    for bucket, words in enumerate(buckets):
        for w in words:
            centibels.setdefault(w, bucket)
    return centibels


def weight_of(count: float) -> int:
    return max(1, round(math.log10(max(count, 1)) * 1000))


def centibel_count(bucket: int) -> float:
    """Words per WORDFREQ_SCALE in wordfreq bucket ``bucket``."""
    return WORDFREQ_SCALE * 10 ** (-bucket / 100)


def bigram_shard(
    args: tuple[str, int, int, tuple[int, str], frozenset[str]],
) -> tuple[int, Counter]:
    base, shards, index, (size, sha), vocab = args
    url = f"{base}/2-{index:05d}-of-{shards:05d}.gz"
    raw = io.BufferedReader(open_verified(url, size, sha), buffer_size=1 << 20)
    counts: Counter[tuple[str, str]] = Counter()
    with gzip.open(raw, "rb") as lines:
        for line in lines:
            # Only the n-gram is decoded; almost every line is rejected there.
            head, _, rest = line.partition(b"\t")
            w1, sep, w2 = head.decode("utf-8", "replace").lower().partition(" ")
            if not sep or w1 not in vocab or w2 not in vocab:
                continue
            n = year_total(rest.decode("ascii").rstrip("\n").split("\t"))
            if n:
                counts[(w1, w2)] += n
    while raw.read(1 << 20):
        pass
    kept = Counter({k: v for k, v in counts.items() if v >= MIN_SHARD_PAIR_COUNT})
    return index, kept


@dataclass(frozen=True)
class Language:
    code: str
    vocabulary: Callable[[], set[str]]
    # The only one-letter words; wordfreq also counts stray letters and
    # elided articles ("l'", "d'") that must not become suggestions.
    single_letters: frozenset[str]
    max_words: int
    max_bigrams: int
    ngram_corpus: str
    bigram_lock: dict[int, tuple[int, str]]


LANGUAGES = {
    "en": Language(
        code="en",
        vocabulary=english_vocabulary,
        single_letters=frozenset({"a", "i"}),
        max_words=MAX_WORDS_EN,
        max_bigrams=40_000,
        ngram_corpus="eng-fiction",
        bigram_lock=BIGRAM_LOCK_EN,
    ),
    "fr": Language(
        code="fr",
        vocabulary=french_vocabulary,
        single_letters=frozenset({"a", "à", "y", "ô"}),
        max_words=MAX_WORDS_FR,
        max_bigrams=40_000,
        ngram_corpus="fre",
        bigram_lock=BIGRAM_LOCK_FR,
    ),
}


def generate(lang: Language, out: Path, jobs: int, words_only: bool) -> None:
    vocab = {w for w in lang.vocabulary() if len(w) > 1 or w in lang.single_letters}
    centibels = wordfreq_centibels(lang.code)
    ranked = sorted(
        (
            (w, weight_of(centibel_count(cb)))
            for w, cb in centibels.items()
            if w in vocab
        ),
        key=lambda kv: (-kv[1], kv[0]),
    )[: lang.max_words]
    (out / f"words-{lang.code}.txt").write_text(
        "".join(f"{w}\t{weight}\n" for w, weight in ranked), encoding="utf-8"
    )
    print(
        f"words-{lang.code}.txt: {len(ranked)} words, rarest weight {ranked[-1][1]}",
        file=sys.stderr,
    )
    if words_only:
        return

    dictionary = frozenset(w for w, _ in ranked)
    base = f"{NGRAM_ROOT}/{lang.ngram_corpus}"
    shards = len(lang.bigram_lock)
    pairs: Counter[tuple[str, str]] = Counter()
    with ProcessPoolExecutor(max_workers=jobs) as pool:
        work = [
            (base, shards, i, lang.bigram_lock[i], dictionary) for i in range(shards)
        ]
        for index, shard in pool.map(bigram_shard, work):
            print(
                f"  {lang.code} 2-gram shard {index}: {len(shard)} pairs",
                file=sys.stderr,
            )
            pairs.update(shard)
    top = sorted(pairs.items(), key=lambda kv: (-kv[1], kv[0]))[: lang.max_bigrams]
    top.sort(key=lambda kv: (-weight_of(kv[1]), kv[0]))
    (out / f"bigrams-{lang.code}.txt").write_text(
        "".join(f"{a}\t{b}\t{weight_of(c)}\n" for (a, b), c in top), encoding="utf-8"
    )
    floor = min((c for _, c in top), default=0)
    print(
        f"bigrams-{lang.code}.txt: {len(top)} bigrams, rarest counted {floor}",
        file=sys.stderr,
    )


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--out", type=Path, default=HERE)
    parser.add_argument(
        "--lang",
        action="append",
        choices=sorted(LANGUAGES),
        help="language to generate (repeatable; default: all)",
    )
    parser.add_argument(
        "--words-only",
        action="store_true",
        help="rewrite only words-<lang>.txt (skips the 2-gram download)",
    )
    parser.add_argument(
        "--jobs", type=int, default=2, help="parallel bigram shard downloads"
    )
    args = parser.parse_args()
    for code in args.lang or sorted(LANGUAGES):
        generate(LANGUAGES[code], args.out, args.jobs, args.words_only)


if __name__ == "__main__":
    main()

# Swipe decoder fixtures

Test-only data; nothing here is embedded in the keyboard.

| File                 | Contents |
|----------------------|----------|
| `swipe_tune.txt`     | 2 000 English swipes to fit swipe decoder constants on. |
| `swipe_eval.txt`     | 1 500 swipes from other contributors: the held-out accuracy fixture (`swipe::tests::futo_held_out_accuracy`). Never fit on it. |
| `generate.py`        | Deterministic generator for both files. |
| `LICENSE.FUTO-SWIPE` | The dataset's MIT licence and copyright notice, verbatim. |

## Source and licence

`swipe_tune.txt` and `swipe_eval.txt` are **MIT**, *Copyright (c) 2025 FUTO*
(full notice in `LICENSE.FUTO-SWIPE`): samples of the `dev.jsonl` and
`test.jsonl` swipes of FUTO's swipe.futo.org dataset
(<https://huggingface.co/datasets/futo-org/swipe.futo.org>, revision
`d71bf5fd7f45b3e7c2ed2d76a21b0dbd3b4ba566`), remapped to the OSK's QWERTY key
geometry and thinned. The two splits share no contributor session.
`generate.py` is part of Tilo (AGPL-3.0-only).

## Regenerating

```sh
uv run apps/osk/tests/data/generate.py
```

downloads the pinned dataset files (~575 MB, SHA-256 checked, cached in
`~/.cache/tilo-swipe-data`) and rewrites both fixtures byte-identically.
Regenerate them, and recheck the accuracy floors, after regenerating
`data/words-en.txt`: the fixtures keep only words in it.

```sh
cargo test --release -p osk --lib futo_held_out -- --nocapture
```

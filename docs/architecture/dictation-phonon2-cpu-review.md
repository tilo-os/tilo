# Phonon-2 CPU source review

The exact published `fermion-research` 0.2.11 wheel contains a local-only
packed CPU path. This is source evidence, not an inference or streaming
qualification. Its [integrity receipt](dictation-phonon2-cpu-review.json)
records the wheel digest, 112 verified wheel entries, 20 verified native
library pins, inspected source hashes and the current immutable image identity.
The package was extracted for inspection; it was never imported or installed.

Passing a complete local model directory bypasses the CLI's model fetch.
The CLI checks `config.json` and `packed_manifest.json`; the CPU loader also
requires `model.fermion`. The packed path uses local architecture/frontend
code and vocabulary, with native C encoder and TDT kernels. It does not need
teacher weights, teacher configuration or tokenizer downloads. Some tensors
still expand, and native loading, container reading or C-engine failures can
enter dense expansion before later refusal. No strict-native-only flag was
found. Neither archive size nor successful transcription proves a 1 GiB fit.

The wheel's `listen --wav` entrypoint explicitly requires and loads MLX.
It cannot qualify Linux CPU previews. Shared live code accepts the CPU speech
model contract, but its 0.35-second initial audio threshold and 0.5-second
increments are scheduling thresholds, not observed preview latency. A later
private harness must measure useful previews, not just matching hypotheses.

The current CPU recipe names image tag 2.0.9 and package 0.2.11. Existing
[upstream pins](dictation-upstream-pins.json) identify an older image and remain
unchanged. Image metadata alone does not prove native source/build provenance;
the wheel contains Python loaders but no corresponding C/C++ kernel sources.
Its NOTICE licenses packaged native binaries under Apache-2.0. Weights have
separate CC-BY-4.0 attribution and NOTICE requirements.

Actual qualification needs a pinned CPU dependency closure, verified local
model, network denial, read-only inputs, two-CPU affinity/two threads and a
hard 1 GiB cgroup with zero swap. Clear native-library overrides, require actual
native encoder/TDT status, reject fallback, observe memory events and retain
failed runs. Start with local-file transcription, then qualify paced live
preview and cancel/restart independently. Missing observations must fail.

Automatic approval review rejected acquisition of the 418 MB runtime image
because persistent builder storage requires approval. No acquisition or
workaround ran. No model, microphone, service, trust or production setting
changed. Linux CPU inference, memory and preview performance remain untested.

Sources: [published package metadata](https://pypi.org/pypi/fermion-research/0.2.11/json),
[exact wheel](https://files.pythonhosted.org/packages/35/c3/90dc124286ce01004ddb30cd302381aad79fc281899d7dd2973ec85faef6/fermion_research-0.2.11-py3-none-any.whl),
[pinned CPU recipe](https://github.com/fermionresearch/phonon/blob/cf97fdbedaf5bfd3cc8b88c946e64c0fdc2587b1/docker-cpu-phonon2/Dockerfile),
[CPU documentation](https://github.com/fermionresearch/phonon/blob/cf97fdbedaf5bfd3cc8b88c946e64c0fdc2587b1/docs/cpu.md).

# Conservative preview-context screen

Public first exposed FLEURS English/French/German clips, existing MIT Small Q5_1 model, two CPUs, 1 GiB address-space/container-memory bounds. No hardware microphone, network inference, installation, persistent grants or reserved holdout decoding.

Context 1250 reduces native prefix decode time by 20.5% at the median. Eight of nine paired previews match; one short English preview changes. Both policies still produce inaccurate short previews. The independently decoded full-context finals have French 2/20, English 1/19 and German 0/18 word errors. These results do not qualify a production preview policy or whole-worker latency improvement.

The archive retains failed SELinux and container-ulimit launches, exact successful source/library hashes, driver and launcher, raw-log identities and lossless normalized stdout. Identical artifacts share canonical files. Model weights, audio, native archives and binaries are not redistributed. FLEURS attribution: Google FLEURS dataset authors, CC-BY-4.0; decoder upstream: Whisper.cpp MIT. Project wrapper/driver source follows the repository license.

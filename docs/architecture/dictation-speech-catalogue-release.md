# Reviewed speech image inputs — 2026-10-10

Protected [review run 91547](https://code.tilo.so/tilo/tilo/actions/runs/91547)
completed successfully at source commit
`5d29fb38e8e76bc5e3048a19c5503e1a2015a8ca`. Its exact source and signed
catalogue are now the image inputs. The existing public key remains byte for
byte unchanged. `dist/local-models/speech-catalog-review-20261010.json` retains
the artifact hashes and independent production-domain Ed25519 verification.
The signed payload preserves U²-Net and adds the pinned multilingual Whisper
Tiny, Base and Small Q5_1 releases; every model field was compared with the
reviewed source. The compiler sorts model entries by identifier.

This source promotion does not publish weights, install a model, accept a
person's licence or record audio. Dispatch the existing `publish` mode after
protected integration to reproduce these exact bytes and deliver the immutable
weight objects and complete MIT notices. Normal OS delivery is still separate.

After the catalogue and objects are delivered, explicit installation of the
qualified Small release is:

```sh
tilo-model-manager install --model whisper-small-q5_1-multilingual \
  --revision 5359861c739e955e79d9a303bcbc70fb988958b1 --accept-license
```

That records MIT acceptance for the exact revision and SHA-256 and queues the
190085487-byte verified download. The most recently installed intact speech
model is used by the recognizer. Installation neither starts a runtime nor
grants microphone access. Removing this newly installed model through the
normal manager removes its object and model entry, while retaining its licence
acceptance receipt; earlier installed models remain intact.

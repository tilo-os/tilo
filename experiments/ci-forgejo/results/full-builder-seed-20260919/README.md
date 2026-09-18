# Normal full-builder runner pilot

Repository `qos/qos` (23), isolated branch `codex/ci-full-builder-seed-probe`,
commit `d575c611be844d2c709e869582c705e2cc431363`. This adds only the retained
workflow on its own branch; no product-main change. Dispatch occurred after
infra cleared the storage/runtime hold. It uses generic `runs-on: runner` and
the full builder pinned to `027e43c051f1f34ce9ac1539172af0e711958243115e2efd08901ffa58729c5a`.
There is no node forcing, special label, prewarm hook, checkout or artifact upload.

Run 22744 (UI index 9302), job 83504, task 75046 completed successfully.
Both native C and Rust programs compiled and ran, and the named native library
pkg-config checks passed. `final.json` and selected log lines retain that result.
Runner API identity 14993 / `ci-warm-46d2f177e1874951aeb2` is repository-scoped:
repo 23, owner 0, generic labels. This is real broker-admitted Actions execution.

| Interval | Observed duration |
| --- | ---: |
| Run created to run started | 614 s |
| Run started to stopped | 240 s |
| Image pull requested to container create | 236.876 s |
| Native smoke start to finish (log timestamps) | 0.160 s |
| Run created to container create | 852.604 s |

The jobs API does not expose job creation time, so exact job-created-to-container
latency remains unknown. The run-created interval is explicitly distinguished.
The pull-to-create interval includes authentication, registry and container
setup; it is not an isolated network-download measurement.

This run does **not** establish a seed cache hit. Initial infra correlation found
no corresponding on-prem pod, suggesting a cloud cold fallback. A subsequent
bounded cloud lookup by the infrastructure observer returned no pod for that
job name after cleanup; its node/PVC association was not retained. Therefore
cloud placement is an inference, not an established fact in this evidence. Other normal
qOS jobs did obtain snapshot-backed on-prem workers, but those jobs must not be
substituted as proof that this full-builder job used the snapshot. Qualification
still needs a full-builder execution bound to the actual snapshot PVC and cached
image on its runner.

Operational note: a broad `/actions/runs?limit=8` query stalled. Its exact local
client process was terminated and an exact full `head_sha` query returned promptly.
No workflow was canceled or redispatched. The same push also scheduled existing
lint workflow run 22745; it was preserved.

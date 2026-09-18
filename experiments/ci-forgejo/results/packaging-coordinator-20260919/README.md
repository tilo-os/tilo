# Live coordinator qualification

Two private nonpublishing fixture runs completed successfully on 2026-09-18 UTC.
The actual CLI used a durable SQLite database and separate processes for each
observation/tick. No product workflow, tag, release, protection or schedule changed.

1. Observe immutable source A, then dispatch it: run **22827**, job **83831**,
   [pilot UI 18](https://code.podesta.ai/qos/ci-workflow-pilot-20260918-9245-repeat/actions/runs/18).
2. While A was queued, advance its fixture branch twice and observe B, then C.
   `pending-C-active-A.json` shows A preserved as active, C latest desired, and no
   successful baseline yet. The original SHA remained immutable despite movement.
3. Poll A to success, advancing the baseline to A. A later tick dispatches C,
   carrying A's SHA as its baseline: run **22842**, job **83881**,
   [pilot UI 19](https://code.podesta.ai/qos/ci-workflow-pilot-20260918-9245-repeat/actions/runs/19).
4. Poll C to success: the final state is idle, desired and successful baseline C.

The two retained attempt records are exactly A and C; the coordinator never
dispatched B. Server records and selected job log receipts independently agree on
both immutable source SHAs, persisted intent UUIDs and baseline inputs. Forgejo
reports `event=""` and `trigger_event="workflow_dispatch"`, handled explicitly.
Run A's printed baseline is `none`; run C's is successful A, not previous push B.

```sh
python experiments/ci-forgejo/verify-packaging-coordinator-evidence.py
python -m unittest discover -s tools/tests -p test_app_packaging_coordinator.py -v
```

Both checks pass. Restart, uncertain POST responses, failure without retry and
concurrent-worker claims are covered by the ten local regression tests; the live
pilot did not deliberately kill a process or inject network failure.

This proves coordinator dispatch/coalescing and successful-baseline semantics,
not application compilation, dependency graph correctness or durable publication.
The tiny diagnostic Python image uses a mutable tag. Production publication still
requires separate qualification and deliberate activation. The database remains
uncommitted in the isolated checkout's `dist/coordinator-live/`; JSON snapshots
retain reviewable state without treating a copied database as a second controller.

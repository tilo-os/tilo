# Clip single-group validation v1

Root approved review-freeze-2 for validation. The version manifest covers 34 source/config/evidence inputs. The final plan has four lint checks and sixteen runtime commands. The two vendor suites explicitly execute the new non-ignored shared-input, route-boundary and oversized-guard tests; the native full/sparse pixel proof is separately invoked with --ignored under C and BPC. Existing native shell retained/dense and staging/history oracles remain unchanged. CPU/config guard returns and ignored tests will be audited separately from executed GPU coverage.

Source owners remain frozen until every running command terminates. Stop on any failed check, retain its log, and repair only in a new recorded version. Passing this plan does not establish speed, lower memory usage, default acceptance, or the overall goal. No commit/push is authorized by validation alone.

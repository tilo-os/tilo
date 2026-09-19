# Platform boundary guard implementation plan

> **For agentic workers:** Use subagent-driven-development for task execution and independent task review.

**Goal:** Prevent new direct native Rust and native dependency leaks in applications while the complete framework migration proceeds.

**Architecture:** A repository policy checker scans app source and manifests, excluding framework implementations. Exact existing sites have shrinking, attributed migration exceptions. The checker supplements, and does not replace, later public API and target-resolved dependency checks.

**Tech stack:** Python 3.11+ standard library, unittest, TOML/JSON, existing Forgejo quality workflow.

**Spec:** `docs/superpowers/plans/2026-09-18-cross-platform-framework/README.md` and `review.md`; active user goal is binding.

## Global constraints

- Framework internals must contain platform-specific implementations. Only app-side native implementation is restricted.
- Preserve all pre-existing work. Only explicitly listed files belong to this task.
- Never weaken security or assertions; no silent skip when a subject cannot be observed.
- Do not change Cargo manifests/lockfiles or app behavior for this policy-only task.
- Existing migration exceptions are not portability certification. G03/G04 remain incomplete until API/graph/feature enforcement and migrations finish.
- Use Python stdlib only, and reuse existing helper behavior only when its parser is adequate.

### Task 1: Ratchet direct native source and manifest dependencies

**Files:**
- Create `tools/check_app_platform_contract.py`.
- Create `tools/app_platform_baseline.json`.
- Create `tools/tests/test_app_platform_contract.py`.
- Modify `.forgejo/workflows/lint.yml` to run the new checker after the existing application contracts.

**Interfaces:**
- Provide `audit(root: Path)` returning violations and a checked-app count, following adjacent policy-checker conventions. Each violation identifies path, line, rule and actionable reason.
- CLI without arguments audits the repository using its checked-in baseline; exit 0 means no new leaks or stale exceptions, nonzero means violations/configuration error. Provide `--root PATH` for test fixtures and an explicit inventory command/flag that prints detected sites for human review; normal audit never modifies baseline.
- Baseline entries identify exact relative path, rule, normalized source/dependency fingerprint and allowed occurrence count, with audit finding IDs and owning removal task. Line numbers are diagnostic hints, not identity: moving an unchanged normalized site within its file retains its exception. Missing files/sites and decreased occurrence counts produce stale-exception failures. Changed sites/new occurrences are not covered by another site's count. Reject malformed/duplicate entries and nonexistent finding/task IDs against the migration register. Fixture roots provide their own minimal finding register.

**Scope of initial rules:**
- Scan application Rust sources recursively below app packages, including standalone nested app tools. Exclude generated `target`, VCS and dependency caches; framework source is outside this scope.
- Detect `std::os`, native FFI declarations, direct calls/import paths to `libc`, `nix`, `rustix`, `windows_sys`, `windows`, `ashpd`, `zbus`, `xattr` and explicit OS/family conditionals (`target_os`, `target_family`, `cfg(unix)`, `cfg(windows)`, including cfg!/cfg_attr forms). Resolve grouped imports and straightforward renamed imports. Native test sites remain inventoried and require specific attributed exceptions, not whole-test/app exemptions.
- Ignore comments and ordinary/raw Rust string contents as code; byte strings and nested block comments must not produce phantom APIs. Platform command strings and host-path ownership will get separate contextual rules; do not claim this first lexical gate detects every platform mechanism.
- Inspect normal, build, dev and target-specific Cargo dependency tables, nested manifests and workspace-inherited/renamed dependencies. Reject direct native mechanism dependencies from the above list and `runtime-portal-xdg`. Detect app feature selection of SDK/backend features `webview-wpe`, `dmabuf-import`, `mpris`, including `ui/FEATURE`, `ui?/FEATURE`, renamed SDK dependencies and dependency `features` arrays. Do not reject semantic `portal` merely for its name. Resolve inherited workspace feature arrays as well as package names.
- Permit portable std file I/O, file-format strings, ordinary cfg(test), and native framework source. Detect facts, do not decide that all use of a native-sounding word in a comment is an error.

- [ ] **Step 1: Write failing fixture tests through production audit and CLI paths.** Include these behaviors with real temporary package trees:

```python
def test_framework_backend_is_allowed_but_app_native_import_fails(self):
    root = self.fixture('use std::os::unix::fs::PermissionsExt;')
    backend = root / 'crates/system/example/src/lib.rs'
    backend.parent.mkdir(parents=True)
    backend.write_text('use std::os::unix::fs::PermissionsExt;')
    violations, count = contract.audit(root)
    self.assertEqual(count, 1)
    self.assertTrue(any(v.rule == 'native-api' for v in violations))
    self.assertTrue(all('apps/' in v.path.as_posix() for v in violations))
```

Test comment/raw-string false positives; grouped/aliased native imports;
OS cfg variants; renamed/inherited native dependencies in target/dev/build
tables; optional SDK feature syntax; nested profile-tool manifest; exact baseline
acceptance, extra same-site occurrence, same-rule different site, moved line,
deleted site/file, invalid finding/task, malformed/duplicate baseline; CLI
nonzero on failures and inventory not modifying files. Test a renamed SDK
dependency using `package = "tilo-app"` and ensure feature selection is caught.

- [ ] **Step 2: Run** `python3 -m unittest discover -s tools/tests -p 'test_app_platform_contract.py'` and record red evidence showing the absent checker. Implement parser/audit incrementally and rerun focused tests to green. Avoid relying on the adjacent simplistic string masker where Rust raw strings require stronger handling.
- [ ] **Step 3: Inventory the live checkout and write the attributed baseline.** Resolve sites using the audit and register; where current code differs, attach the relevant removal task and explicit rationale. Every exemption is site-specific. Do not add a blanket allowlist of apps, files or entire dependency categories.
- [ ] **Step 4: Add the CI step**, preserving other workflow commands. Run the new checker on the real checkout and the new unittest suite. Run existing resource/action checker unit tests once to ensure no shared parser regression if helpers were reused.
- [ ] **Step 5: Review and report.** Include rules intentionally not yet covered (transitive API/graph/command/path/C++), baseline size, TDD evidence and exact changed files. Parent will arrange independent review and commit only task-owned changes after review; worker must not commit pre-existing work or spawn agents.

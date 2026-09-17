# Final whole-feature review

## Verdict

**Needs one fix before ready.** The final combined implementation meets the shared authority, all-app launch placement, lifecycle, descendant, privacy, and observed native behavior requirements. One Important first-launch bootstrap defect remains when ordinary Cargo configuration selects an explicit native target triple. The retained native-3 result remains valid for its tested configuration.

## Scope and method

Reviewed the final combined result represented by `final-review.diff`: implementation `c2d6045c8` plus `6dce082e8`, proof/docs `e01fc4809` plus `9c6de88b3`, and relevant Browser privacy/packaging integration `a3720f6ff` and `163c14dad`. Read the binding spec, plan, task reports/reviews, ledger rulings, durable acceptance audit, implementation interfaces and native-3 evidence. Fixed intermediate findings are not repeated. Unrelated concurrent Photos History and Browser UX changes remain excluded.

No source, index, branch or commit changes, no subagents, no GUI launches and no broad test reruns. The sole new focused experiment used a temporary tiny Cargo crate and the existing compiled portal bootstrap fixture to investigate configured-target output discovery. Temporary experiment files were automatically removed. This report is the only retained review artifact.

## Strengths

- `shell/src/runtime_host/unix.rs:55` centralizes descriptor transfer and identity handshake preparation for managed launch and automatic exec. The daemon owns production `Session`; MCP connections reuse existing `McpServer` and broker authority. There is no second action registry, permission model or per-app routing layer.
- `shell/src/runtime_host/daemon.rs:183` serializes startup and probes real service responses; `:245` acquires Session ownership before stale endpoint removal. Client/app exit does not own the shared daemon or other apps. Profile checks, bounded status frames and startup timeout provide meaningful failure boundaries.
- `ui/src/launch.rs:648` and `:755` attach before live construction across definition/default/custom paths, with diagnostics and handled commands excluded. The source coverage inventory describes 47 entry points without misrepresenting them as 47 native executions.
- `crates/runtime/runtime-portal/src/lib.rs:88` shares descendant provenance interpretation with bootstrap. Current/legacy genuine transfers remain authoritative, while ordinary children ignore ancestor-owned descriptor values and obtain their own connections. Fresh transfer preparation clears ancestor resume metadata.
- Browser private mode uses a generic startup policy and a live local constructor, preserving wake ownership while preventing ambient identity/collaboration/presence/continuation discovery. Packaging includes the shared host beside the Browser executable. This is an appropriate explicit local-mode exception rather than a normal-app allowlist.
- The native proof exercises four literal independent Cargo launches, first-app daemon startup, distinct peers, late Calculator joining, canonical action success and survival after Writer exit. The revised proof exposes source snapshot preparation and rejects failed profile removal. Documentation distinguishes automatic app joining from explicit external assistant configuration and declares platform limits.

## Critical findings

None identified.

## Important findings

### [P2] Discover the actual Cargo executable when a target triple is configured

**Location:** `crates/runtime/runtime-portal/src/native.rs:229` (also `:253`).

`build_host` unconditionally expects `TARGET/debug/qos-runtime-mcp`, but its Cargo child retains ordinary `CARGO_BUILD_TARGET` and repository/user `[build].target` configuration. Cargo writes a successful explicitly targeted build to `TARGET/<triple>/debug/qos-runtime-mcp`, including when `<triple>` is the current native host. In a source checkout with the host absent, a perfectly ordinary `cargo run -p <app>` under that configuration therefore builds the runtime successfully and then fails startup with `native runtime build failed: exit status: 0`. On retries it repeats the same failure. The first missing-host startup requirement and preservation of ordinary Cargo settings are not met for this supported configuration.

**Focused evidence:** A real tiny dependency-free Cargo build with `CARGO_BUILD_TARGET=aarch64-apple-darwin` and an explicit target directory exited 0 and reported its executable under `actual-target/aarch64-apple-darwin/debug/target-layout-probe`. The existing `target/debug/deps/native-27135d3c2170050e --exact bootstrap_child --nocapture`, with a controlled Cargo producing that verified directory layout, exited 21 and printed the error above. This was not a native GUI launch or a re-run of the whole portal suite.

**Requested correction:** Resolve the built executable from Cargo's actual artifact output or another configuration-aware source, maintaining bounded serialized construction and ordinary user Cargo configuration. Ensure subsequent bootstrap invocations can reuse the selected executable. Add a focused regression for a configured native target triple, including configuration-file selection if the solution only inspects environment variables; retain the existing default-layout/offline/concurrent-build checks.

## Minor findings

None identified that warrant expanding this final fix wave. Existing disclosed unrelated lint warnings remain a validation limitation, not a newly introduced issue.

## Requirement assessment

| Requirement | Assessment |
| --- | --- |
| Shared normal default/custom/definition app startup | Pass by shared placement, launch tests and source inventory |
| Existing broker/identity/MCP authority; no parallel registry | Pass |
| Independent lifecycle, reuse, races and stale endpoints | Pass by code and recorded process tests/native evidence |
| Inherited transfer precedence and ordinary descendant reattachment | Pass after reviewed provenance fix |
| Command handlers once; preserved argv/cwd | Pass by shared resume logic and recorded subprocess tests |
| Diagnostics and intentional private/local modes remain detached | Pass within scoped regression evidence; separate real Browser private GUI acceptance is not claimed |
| Missing-host bootstrap under ordinary Cargo settings | **Needs fix: configured target output discovery** |
| Four ordinary real app launches, late join, real action, survivor proof | Pass for native-3 macOS configuration |
| Reusable proof/docs and cleanup success gate | Pass for reviewed supported workflow; native-3 cleanup remains valid |
| Platform claims | Honest: macOS observed; Linux edge implemented but unexecuted; Windows unsupported |
| YAGNI / shared code / platform edges | Appropriate architecture; no broader rewrite requested |

Recorded focused results inspected: portal 13, UI launch 32, runtime host 36 and successful host build. Task2's eight-test result and scoped fix review were inspected as supplied evidence. Native-3 contains peer IDs 1–4, Calculator terminal `succeeded`, Writer exit survival, MCP exit 0 and six exact known PIDs absent; the controller independently verified the temporary profile absent. The earlier cleanup failures are retained and not counted as acceptance runs. No fresh whole-suite success claim is made by this review.

## Reproduction command and observed output

Run from the repository root; the temporary directory is removed when the command exits. The controlled Cargo phase isolates the existing bootstrap path and produces the directory layout established by the real Cargo phase.

```python
import os, pathlib, subprocess, tempfile, json
repo = pathlib.Path.cwd()
with tempfile.TemporaryDirectory(prefix="qos-review-target-") as directory:
    root = pathlib.Path(directory)
    triple = next(line.split(": ", 1)[1] for line in
                  subprocess.check_output(["rustc", "-vV"], text=True).splitlines()
                  if line.startswith("host: "))
    crate = root / "crate"
    (crate / "src").mkdir(parents=True)
    (crate / "Cargo.toml").write_text(
        '[package]\nname="target-layout-probe"\nversion="0.1.0"\nedition="2021"\n')
    (crate / "src/main.rs").write_text("fn main() {}\n")
    env = dict(os.environ, CARGO_BUILD_TARGET=triple)
    check = subprocess.run(
        ["cargo", "build", "--offline", "--target-dir", str(root / "actual-target"),
         "--message-format=json"], cwd=crate, env=env, text=True, capture_output=True)
    artifacts = [json.loads(line).get("executable") for line in check.stdout.splitlines()
                 if line.startswith("{") and json.loads(line).get("executable")]
    print("Actual Cargo native target result:", check.returncode, artifacts)
    binary_dir = root / "bin"
    binary_dir.mkdir()
    (binary_dir / "cargo").write_text(
        '#!/bin/sh\n/bin/mkdir -p "$CARGO_TARGET_DIR/$CARGO_BUILD_TARGET/debug"\n'
        'printf "#!/bin/sh\\nexit 0\\n" > "$CARGO_TARGET_DIR/$CARGO_BUILD_TARGET/debug/qos-runtime-mcp"\n'
        '/bin/chmod 700 "$CARGO_TARGET_DIR/$CARGO_BUILD_TARGET/debug/qos-runtime-mcp"\n')
    (binary_dir / "cargo").chmod(0o700)
    for key in list(env):
        if key.startswith("QOS_"):
            env.pop(key)
    env.update(PATH=str(binary_dir), CARGO_TARGET_DIR=str(root / "bootstrap-target"),
               QOS_BOOTSTRAP_TEST_CHILD="1")
    test = subprocess.run(
        [str(repo / "target/debug/deps/native-27135d3c2170050e"),
         "--exact", "bootstrap_child", "--nocapture"],
        env=env, cwd=repo, text=True, capture_output=True)
    print("Existing bootstrap subprocess result:", test.returncode)
    print(test.stdout, test.stderr)
```

Observed output (temporary path shortened):

```text
Actual Cargo native target result: 0 ['.../actual-target/aarch64-apple-darwin/debug/target-layout-probe']
Existing bootstrap subprocess result: 21

running 1 test
 qOS: building the shared native runtime (first launch)
BOOTSTRAP ERROR: native runtime build failed: exit status: 0
```

After the targeted correction and scoped review, the existing architecture and native acceptance evidence provide a strong basis for readiness; the final-ready gate remains open now.

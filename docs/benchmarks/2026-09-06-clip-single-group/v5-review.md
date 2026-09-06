# Validation v5

Only vendor lib.rs and UI offscreen.rs changed among the 34 inputs since v4. Root reviewed the missing test-type import and nine simplified test-module qualifications. Production rendering and tests' assertions are unchanged. All 20 checks are rerun. Vendor-test warnings are inspected even when that command returns zero; changed-line/compiler diagnostics remain a separate required gate. v1-v4 failures are retained.

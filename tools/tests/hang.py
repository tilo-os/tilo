"""How long a tools test waits on a subprocess before calling it hung.

The same bound and override as `tilo_test_env::hang_bound()`: a wait that
guards against a hang uses it instead of a timing guess, so a loaded runner
cannot turn a slow run into a failure.
"""

import os

HANG_BOUND_ENV = "TILO_TEST_HANG_BOUND_SECS"

HANG_BOUND = float(int(os.environ.get(HANG_BOUND_ENV, "120").strip()))

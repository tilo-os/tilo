#!/usr/bin/env python3
"""Run the retained SDK qualification against an owned synthetic CEF page.

Usage: mounted_text_input.py CEF_HOST SDK_QUALIFICATION_BINARY
No microphone, personal profile, display, clipboard or page-agent logging.
"""
from pathlib import Path
import http.server
import os
import shutil
import subprocess
import sys
import tempfile
import threading
import time


def main():
    if len(sys.argv) != 3:
        raise SystemExit(__doc__)
    host = Path(sys.argv[1]).resolve(strict=True)
    binary = Path(sys.argv[2]).resolve(strict=True)
    # The shared native fixture reads its host argument at import time.
    sys.argv = [sys.argv[0], str(host)]
    from guarded_text_input import Handler

    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    runtime = tempfile.mkdtemp(prefix="tilo-mounted-dom-")
    try:
        env = dict(os.environ,
                   XDG_RUNTIME_DIR=runtime,
                   XDG_CONFIG_HOME=runtime + "/config",
                   XDG_CACHE_HOME=runtime + "/cache",
                   XDG_DATA_HOME=runtime + "/data")
        env.pop("DISPLAY", None)
        env.pop("WAYLAND_DISPLAY", None)
        subprocess.run([str(binary), str(host), runtime + "/profile",
                        f"http://127.0.0.1:{server.server_port}/input"],
                       env=env, check=True, timeout=180)
    finally:
        server.shutdown()
        server.server_close()
        # CEF storage children can finish after the owning host exits. Cleanup
        # remains bounded and must fail rather than leave an unobserved profile.
        deadline = time.monotonic() + 10
        while Path(runtime).exists():
            try:
                shutil.rmtree(runtime)
            except OSError:
                if time.monotonic() >= deadline:
                    raise
                time.sleep(0.05)


if __name__ == "__main__":
    main()

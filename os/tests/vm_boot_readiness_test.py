"""Exercise vm.sh wait over its production console client, without a real VM."""
import json
import os
from pathlib import Path
import socket
import subprocess
import tempfile
import threading
import unittest

REPO = Path(__file__).resolve().parents[2]


class ReadinessTests(unittest.TestCase):
    def wait_for(self, state, serial=""):
        with tempfile.TemporaryDirectory(prefix="tilo-ready-") as directory:
            root = Path(directory)
            (root / "serial.log").write_text(serial)
            (root / "qemu.pid").write_text(str(os.getpid()))
            # Never connect to a real desktop or guest through the SSH fallback.
            helper = root / "sshpass"
            helper.write_text("#!/bin/sh\nexit 1\n")
            helper.chmod(0o700)
            stopped = threading.Event()
            requests = []
            with socket.socket(socket.AF_UNIX) as server:
                server.bind(str(root / "console.sock"))
                server.listen()
                server.settimeout(0.1)

                def answer():
                    while not stopped.is_set():
                        try:
                            connection, _ = server.accept()
                        except socket.timeout:
                            continue
                        with connection, connection.makefile("rb") as stream:
                            request = json.loads(stream.readline())
                            requests.append(request)
                            reply = {"seq": request["seq"], "state": state}
                            connection.sendall(json.dumps(reply).encode() + b"\n")

                worker = threading.Thread(target=answer)
                worker.start()
                env = dict(os.environ, TILO_DISTROBOX="", TILO_RUN=str(root),
                           TILO_BOOT_WAIT_SECS="1", TILO_SSH_PORT="1",
                           PATH=f"{root}:{os.environ['PATH']}")
                try:
                    result = subprocess.run(["bash", str(REPO / "os/dev/vm.sh"), "wait"],
                                            env=env, capture_output=True, text=True, timeout=15)
                finally:
                    stopped.set()
                    worker.join(timeout=2)
                    self.assertFalse(worker.is_alive(), "console fixture did not stop")
            self.assertTrue(requests, "vm.sh never observed the console")
            self.assertTrue(all(request["op"] == "state_dump" for request in requests))
            return result

    def test_quiet_greeter_without_ssh_accepts_input(self):
        result = self.wait_for({"app": "greeter", "accepts_input": True})
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("greeter accepts input", result.stderr)

    def test_quiet_installer_without_ssh_answers(self):
        result = self.wait_for({"app": "installer", "step": "welcome"})
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("installer up", result.stderr)

    def test_started_greetd_with_unready_greeter_fails(self):
        result = self.wait_for({"app": "greeter", "accepts_input": False},
                               "Started greetd.service - Greeter daemon.\n")
        self.assertNotEqual(result.returncode, 0)
        self.assertNotIn("greeter accepts input", result.stderr)


if __name__ == "__main__":
    unittest.main()

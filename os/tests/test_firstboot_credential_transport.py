#!/usr/bin/env python3
"""Run first-boot account creation with synthetic credentials and inert tools."""

import json
import os
from pathlib import Path
import shlex
import shutil
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
SOURCE = ROOT / "os/usr/libexec/tilo-firstboot-user"
FILES_BINARY = Path(os.environ.get('TILO_FIRSTBOOT_FILES_BINARY', ROOT / 'target/debug/tilo-firstboot-files'))
SYNTHETIC_HASH = "$6$fixture$" + "A" * 86

# Account tools and the seed producer are inert in this credential boundary
# fixture; production parsing and unlock ordering run unchanged. They are Bash,
# like the script that runs them: a boot starts about ten, and a Python
# interpreter for each was most of this suite's time.
STAND_IN = r"""set -u
LC_ALL=C
root=${FIXTURE_ROOT:?}
die() { printf '%s\n' "$1" >&2; exit 1; }
put() { printf %s "$2" > "$1" || exit 1; }
# The arguments as json.dumps writes a list of printable ASCII strings; an
# argument it would escape any other way is refused instead.
json() {
  local arg c i out= sep=
  for arg; do
    case $arg in *[!\ -~]*) die 'argument outside printable ASCII' ;; esac
    out+=$sep\"
    for ((i = 0; i < ${#arg}; i++)); do
      c=${arg:i:1}
      case $c in '"' | '\') out+='\' ;; esac
      out+=$c
    done
    out+=\"
    sep=', '
  done
  REPLY=[$out]
}
"""
TOOLS = {
    "bin/id": "exit 1",
    "bin/stat": r"""case ${2?} in
  %u:%a:%h) echo 0:600:1 ;;
  *) exec "$real_stat" -L -c %s -- "${@: -1}" ;;
esac""",
    "bin/useradd": r"""json "$@"; put "$root/useradd.json" "$REPLY" """,
    "bin/getent": r"""printf '%s\n' "fixtureuser:x:$uid:$gid::$root/home/fixtureuser:/bin/bash" """,
    "bin/chpasswd": r"""json "$@"; put "$root/chpasswd.json" "$REPLY"
IFS= read -r -d '' input || :
put "$root/password-stdin" "$input"
exit "${FIXTURE_PASSWORD_FAILURE-0}" """,
    "bin/chage": r"""[ $# -eq 3 ] && [ "$1" = -E ] && [ "$2" = -1 ] && [ "$3" = fixtureuser ] \
  || die 'unexpected chage arguments'
put "$root/expiry-release" released""",
    "bin/shred": r"""exec rm -- "${@: -1}" """,
    "bin/chown": ":",
    "bin/cp": ":",
    # This inert helper covers unlock ordering only. The separate native
    # seed/adoption fixture uses the actual native executable and real NSS.
    "home-seeds": r"""[ $# -eq 1 ] && [ "$1" = fixtureuser ] || die 'unexpected seed arguments'
put "$root/seeds-called" called
exit "${FIXTURE_SEEDS_FAILURE-0}" """,
}


class CredentialTransportTests(unittest.TestCase):
    def run_firstboot(self, root, full_name, seeded_home, fail_password=False, fail_seeds=False):
        credential = root / "install-user.conf"
        credential.write_text("fixtureuser:" + SYNTHETIC_HASH + "\n")
        credential.chmod(0o600)
        name = root / "full-name"
        if full_name:
            name.write_text("Fixture User\n")
            name.chmod(0o600)
        home = root / "home/fixtureuser"
        if seeded_home:
            home.mkdir(parents=True)
        (root / "home").mkdir(exist_ok=True)
        seeds = root / "home-seeds"
        source = SOURCE.read_text()
        replacements = {
            "CONF=/etc/tilo/install-user.conf": f"CONF={credential}",
            "FULL_NAME_CONF=/etc/tilo/install-user-full-name": f"FULL_NAME_CONF={name}",
            "TRANSACTION=/etc/tilo/install-user.transaction": f"TRANSACTION={root}/install-user.transaction",
            "key=/var/lib/tilo/device-private/keypair.bin": f"key={root}/absent-device-key",
            '"/home/$user"': f'"{root}/home/$user"',
            '"/var/home/$user"': f'"{root}/var-home/$user"',
            'seed_helper=/etc/tilo/firstboot-home-seeds': f'seed_helper={seeds}',
            'FILES_HELPER="$(dirname -- "${BASH_SOURCE[0]}")/tilo-firstboot-files"': f'FILES_HELPER="{FILES_BINARY}"',
        }
        (root / 'home').mkdir(exist_ok=True)
        (root / 'skel').mkdir()
        for original, replacement in replacements.items():
            expected_count = 2 if original in ['"/home/$user"', '"/var/home/$user"'] else 1
            self.assertEqual(source.count(original), expected_count, original)
            source = source.replace(original, replacement)
        script = root / "firstboot"
        script.write_text(source)
        tools = root / "bin"
        tools.mkdir()
        values = {"real_stat": shutil.which("stat"), "uid": os.getuid(), "gid": os.getgid()}
        self.assertTrue(values["real_stat"], "the stat stand-in asks the real stat for a size")
        prelude = "#!/usr/bin/env bash\n" + "".join(
            f"{name}={shlex.quote(str(value))}\n" for name, value in values.items()) + STAND_IN
        for tool, body in TOOLS.items():
            path = root / tool
            path.write_text(prelude + body + "\n")
            path.chmod(0o755)
        # Nothing in the script runs sync today; its stand-in is as it was.
        path = tools / "sync"
        path.write_text(
            "#!/usr/bin/env python3\nimport json, os, sys\nfrom pathlib import Path\n"
            "root = Path(os.environ['FIXTURE_ROOT'])\n"
            "fd = os.open(sys.argv[-1], os.O_RDONLY); os.fsync(fd); os.close(fd)\n"
        )
        path.chmod(0o755)
        environment = dict(os.environ, PATH=f"{tools}:{os.environ['PATH']}",
                           FIXTURE_ROOT=str(root), FIXTURE_PASSWORD_FAILURE=str(int(fail_password)),
                           FIXTURE_SEEDS_FAILURE=str(int(fail_seeds)))
        result = subprocess.run(["bash", str(script)], env=environment, capture_output=True, text=True)
        arguments = json.loads((root / "useradd.json").read_text())
        self.assertNotIn(SYNTHETIC_HASH, arguments, "password hashes must not be process arguments")
        self.assertRegex(arguments[arguments.index("-p") + 1], r"^!tilo-firstboot-[a-f0-9]{32}$")
        self.assertEqual(arguments[arguments.index('-e') + 1], '1970-01-02')
        self.assertEqual((root / 'expiry-release').exists(), not (fail_password or fail_seeds))
        self.assertIn("-M", arguments)
        self.assertNotIn("-m", arguments)
        self.assertEqual((root / "seeds-called").read_text(), "called")
        self.assertEqual("-c" in arguments, full_name)
        self.assertNotIn(SYNTHETIC_HASH, result.stdout + result.stderr)
        if fail_seeds:
            self.assertFalse((root / "chpasswd.json").exists())
            self.assertFalse((root / "password-stdin").exists())
            return result, credential
        self.assertEqual(json.loads((root / "chpasswd.json").read_text()), ["--encrypted"])
        self.assertEqual((root / "password-stdin").read_text(), "fixtureuser:" + SYNTHETIC_HASH + "\n")
        return result, credential

    def test_hash_uses_stdin_for_every_account_creation_branch(self):
        for full_name in [False, True]:
            for seeded_home in [False, True]:
                with self.subTest(full_name=full_name, seeded_home=seeded_home):
                    with tempfile.TemporaryDirectory() as work:
                        result, credential = self.run_firstboot(Path(work), full_name, seeded_home)
                        self.assertEqual(result.returncode, 0, result.stderr)
                        self.assertFalse(credential.exists())

    def test_seed_failure_never_installs_password_or_discards_repair_seed(self):
        with tempfile.TemporaryDirectory() as work:
            result, credential = self.run_firstboot(Path(work), False, False, fail_seeds=True)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("account remains locked", result.stderr)
            self.assertNotIn("created account", result.stdout)
            self.assertTrue(credential.exists())

    def test_password_install_failure_never_creates_an_unlocked_account(self):
        with tempfile.TemporaryDirectory() as work:
            result, _ = self.run_firstboot(Path(work), False, False, fail_password=True)
            self.assertNotEqual(result.returncode, 0)
            self.assertNotIn("created account", result.stdout)


if __name__ == "__main__":
    unittest.main()

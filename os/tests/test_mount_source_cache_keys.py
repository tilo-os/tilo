#!/usr/bin/env python3
"""A RUN that bind-mounts a pulled image's stage names that image in its cache key.

Buildah does not hash a `RUN --mount=type=bind,from=<stage>` source into the
layer's cache key when the stage is only an alias of a pulled image
(`FROM ${RPMS_REF} AS rpms`): a new RPMS_REF reused every layer installed from
the previous snapshot, so a local image kept packages the current snapshot no
longer carries. A stage that runs instructions is rebuilt when its image
changes, and Buildah then invalidates the layers mounting it. Redeclaring the
reference's ARG in the consuming stage puts its value into the history Buildah
compares, so these files must declare it before the first such RUN.
"""
from pathlib import Path
import re
import subprocess
import unittest

REPO = Path(__file__).resolve().parents[2]
VARIABLE = re.compile(r'\$\{?([A-Za-z_][A-Za-z0-9_]*)')
MOUNT_FROM = re.compile(r'--mount=\S*?\bfrom=([^,\s]+)')


def containerfiles():
    listed = subprocess.run(['git', 'ls-files', '--', 'os'], cwd=REPO, check=True,
                            capture_output=True, text=True).stdout.split()
    return [REPO / name for name in listed if re.search(r'(^|/)Containerfile(\.[^/.]+)?$', name)
            and not name.endswith('.dockerignore') and '/fixtures/' not in name]


def instructions(text):
    """Logical instructions with the physical line each starts on."""
    current, start = '', 0
    for number, raw in enumerate(text.splitlines(), 1):
        line = raw.strip()
        if line.startswith('#') or (not line and not current):
            continue
        continued = line.endswith('\\')
        if continued:
            line = line[:-1].rstrip()
        if not current:
            start = number
        current = f'{current} {line}' if current else line
        if not continued:
            yield start, current
            current = ''
    if current:
        yield start, current


def violations(text):
    """(line, stage, mounted stage, missing ARG) for each uncovered mount."""
    image_aliases = {}  # stage name -> variables of the pulled image it only names
    found = []
    stage, declared, base_variables, alias = None, set(), set(), None
    for line, instruction in instructions(text):
        words = instruction.split()
        keyword = words[0].upper()
        if keyword == 'FROM':
            image = next(word for word in words[1:] if not word.startswith('--'))
            stage = words[-1] if len(words) >= 4 and words[-2].upper() == 'AS' else None
            variables = set(VARIABLE.findall(image)) | image_aliases.get(image, set())
            declared, base_variables = set(), variables
            alias = stage if variables else None
            if alias:
                image_aliases[alias] = variables
            continue
        if alias:
            image_aliases.pop(alias, None)  # it runs instructions, so Buildah rebuilds it
            alias = None
        if keyword == 'ARG':
            declared.update(word.split('=', 1)[0] for word in words[1:])
        elif keyword == 'RUN':
            for mounted in MOUNT_FROM.findall(instruction):
                for variable in sorted(image_aliases.get(mounted, set()) - declared - base_variables):
                    found.append((line, stage, mounted, variable))
    return found


class MountSourceCacheKeys(unittest.TestCase):
    def test_every_pulled_mount_source_is_in_the_cache_key(self):
        problems = [f'{path.relative_to(REPO)}:{line}: stage {stage or "(unnamed)"} mounts {mounted} '
                    f'without ARG {variable}'
                    for path in containerfiles()
                    for line, stage, mounted, variable in violations(path.read_text(encoding='utf-8'))]
        self.assertEqual(problems, [])

    def test_an_undeclared_snapshot_mount_is_reported(self):
        text = ('ARG BASE_REF\nARG RPMS_REF\nFROM ${RPMS_REF} AS rpms\nFROM ${BASE_REF} AS out\n'
                'RUN --mount=type=bind,from=rpms,source=/repo,target=/repo dnf install -y x\n')
        self.assertEqual(violations(text), [(5, 'out', 'rpms', 'RPMS_REF')])
        declared = text.replace('AS out\n', 'AS out\nARG RPMS_REF\n')
        self.assertEqual(violations(declared), [])

    def test_a_stage_that_runs_instructions_is_rebuilt_instead(self):
        text = ('ARG BASE_REF\nARG TOOL_REF\nFROM ${TOOL_REF} AS tool\nRUN make /out\n'
                'FROM ${BASE_REF} AS out\nRUN --mount=type=bind,from=tool,source=/out,target=/t true\n')
        self.assertEqual(violations(text), [])

    def test_the_consuming_stage_base_already_carries_its_own_image(self):
        text = ('ARG BASE_REF\nFROM ${BASE_REF} AS tools\nFROM ${BASE_REF} AS out\n'
                'RUN --mount=type=bind,from=tools,source=/usr/bin/openssl,target=/o true\n')
        self.assertEqual(violations(text), [])


if __name__ == '__main__':
    unittest.main()

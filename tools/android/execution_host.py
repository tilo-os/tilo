"""Require native Linux execution; cross-build/emulator admission is separate."""
import platform
import sys

from qualify_apk import ARCHES


def add_architecture(parser):
    parser.add_argument('--architecture', choices=ARCHES, default=platform.machine(),
                        help='native Linux execution architecture (default: this host)')


def native_architecture(requested):
    if sys.platform != 'linux' or requested not in ARCHES:
        raise ValueError('Android execution journeys require supported native Linux architecture')
    actual = platform.machine()
    if requested != actual:
        raise ValueError('native Android journey requires ' + requested + '; execution host is ' + actual
                         + '; cross-build or emulator admission is not runtime acceptance')
    return actual

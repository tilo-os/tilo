"""Validate the exact worker's bounded protocol advertisement."""
import json


def advertisement(output):
    if len(output) > 4096:
        raise ValueError('capture protocol advertisement exceeds bounds')
    def unique(pairs):
        value = {}
        for key, item in pairs:
            if key in value:
                raise ValueError('duplicate capture protocol field')
            value[key] = item
        return value
    value = json.loads(output, object_pairs_hook=unique)
    if (not isinstance(value, dict) or set(value) != {'schema', 'worker_protocol'}
            or type(value['schema']) is not int or value['schema'] != 1
            or type(value['worker_protocol']) is not int or value['worker_protocol'] not in (5, 6)):
        raise ValueError('capture worker does not support an admitted protocol')
    return value['worker_protocol']


def for_cohort(output, expected):
    """Bind the actual executable advertisement to the selected build cohort."""
    if type(expected) is not int or expected not in (5, 6):
        raise ValueError('Unsupported capture build cohort')
    actual = advertisement(output)
    if actual != expected:
        raise ValueError('Capture executable differs from selected build cohort')
    return actual

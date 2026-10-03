import base64
import io
import pathlib
import struct
import sys
import unittest
import zlib

sys.path.insert(0, str(pathlib.Path(__file__).parents[1] / 'agent-loop'))
import usage  # noqa: E402


def png(width, height):
    header = struct.pack('>IIBBBBB', width, height, 8, 6, 0, 0, 0)
    chunk = struct.pack('>I', len(header)) + b'IHDR' + header
    return b'\x89PNG\r\n\x1a\n' + chunk + struct.pack('>I', zlib.crc32(b'IHDR' + header))


def jpeg(width, height):
    app0 = b'\xff\xe0' + struct.pack('>H', 16) + b'JFIF\x00' + bytes(9)
    sof = b'\xff\xc0' + struct.pack('>HBHHB', 11, 8, height, width, 1) + bytes(3)
    return b'\xff\xd8' + app0 + sof + b'\xff\xd9'


class UsageTests(unittest.TestCase):
    def test_reads_picture_sizes_from_headers(self):
        self.assertEqual(usage.image_size(png(1024, 768)), (1024, 768))
        self.assertEqual(usage.image_size(jpeg(768, 576)), (768, 576))
        self.assertIsNone(usage.image_size(b'not a picture'))

    def test_image_tokens_follow_pixels_after_the_api_downscale(self):
        self.assertEqual(usage.image_tokens(1024, 768), 1049)
        self.assertEqual(usage.image_tokens(768, 576), 590)
        # Larger than the API keeps: scaled to ~1.15 MP before counting.
        self.assertLessEqual(usage.image_tokens(4000, 3000), 1534)
        self.assertEqual(usage.image_tokens(0, 10), 0)

    def test_measures_text_and_images_of_a_tool_result(self):
        result = {'content': [
            {'type': 'text', 'text': 'x' * 400},
            {'type': 'image', 'mimeType': 'image/jpeg', 'data': base64.b64encode(jpeg(768, 576)).decode()},
        ], 'structuredContent': {'ignored': 'x' * 1000}}
        measured = usage.measure(result)
        self.assertEqual(measured['text_bytes'], 400)
        self.assertEqual(measured['images'][0]['width'], 768)
        self.assertEqual(measured['tokens'], 100 + 590)

    def test_summarises_one_connection(self):
        records = [
            {'method': 'tools/list', 'start': 10.0, 'ms': 5, 'text_bytes': 800, 'images': [], 'wire_bytes': 900},
            {'method': 'tools/call', 'tool': 'tilo_invoke', 'actions': 3, 'start': 11.0, 'ms': 1000,
             'text_bytes': 200, 'images': [{'bytes': 5000, 'tokens': 590}], 'wire_bytes': 7000},
        ]
        summary = usage.summarise(records)
        self.assertEqual(summary['tool_calls'], 1)
        self.assertEqual(summary['actions'], 3)
        self.assertEqual(summary['tokens'], 250 + 590)
        self.assertEqual(summary['wall_s'], 2.0)
        self.assertIn('1 tool calls (3 actions)', usage.format_summary(summary))

    def test_names_batched_calls_by_their_actions(self):
        self.assertEqual(usage.what('tilo_invoke', {'calls': [{'action': 'paint'}, {'action': 'look'}]}), ('calls:paint,look', 2))
        self.assertEqual(usage.what('tilo_query', {}), ('query:root', 0))


if __name__ == '__main__':
    unittest.main()

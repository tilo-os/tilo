"""What an agent's session costs: the tool results it read, in bytes and
estimated tokens, the calls it made and the time they took.

`session.py mcp` relays the MCP endpoint and appends one JSON line per
answered request to target/agent-loop/APP/logs/usage/CONNECTION.jsonl;
`session.py usage APP` and `session.py report APP` summarise those files.

Token estimates are deliberately simple, and the same for every run, so two
runs compare even where the absolute number is off:

  text   ceil(UTF-8 bytes / 4), over the content blocks a model reads. The
         `structuredContent` copy is counted only as wire bytes: clients
         hand the model one of the two.
  image  ceil(width * height / 750), Anthropic's published estimate for
         Claude vision, after the downscale the API applies first (longest
         side at most 1568 px, at most ~1.15 megapixels). The token cost
         depends on pixels, not on the encoding: a JPEG saves bytes on the
         wire, a smaller picture saves tokens.
"""
import base64
import json
import math
import struct

TEXT_BYTES_PER_TOKEN = 4
IMAGE_PIXELS_PER_TOKEN = 750
IMAGE_MAX_SIDE = 1568
IMAGE_MAX_PIXELS = 1_150_000


def text_tokens(byte_count: int) -> int:
    return math.ceil(byte_count / TEXT_BYTES_PER_TOKEN)


def image_tokens(width: int, height: int) -> int:
    if width <= 0 or height <= 0:
        return 0
    scale = min(1.0, IMAGE_MAX_SIDE / max(width, height), math.sqrt(IMAGE_MAX_PIXELS / (width * height)))
    return math.ceil(round(width * scale) * round(height * scale) / IMAGE_PIXELS_PER_TOKEN)


def image_size(data: bytes):
    """(width, height) of a PNG, JPEG or WebP, or None."""
    if data.startswith(b"\x89PNG\r\n\x1a\n") and len(data) >= 24:
        return struct.unpack(">II", data[16:24])
    if data.startswith(b"\xff\xd8"):
        at = 2
        while at + 9 < len(data):
            if data[at] != 0xFF:
                at += 1
                continue
            marker = data[at + 1]
            if marker in (0xD8, 0x01, 0xFF) or 0xD0 <= marker <= 0xD7:
                at += 1 if marker == 0xFF else 2
                continue
            length = struct.unpack(">H", data[at + 2 : at + 4])[0]
            if 0xC0 <= marker <= 0xCF and marker not in (0xC4, 0xC8, 0xCC):
                height, width = struct.unpack(">HH", data[at + 5 : at + 9])
                return width, height
            at += 2 + length
        return None
    if data[:4] == b"RIFF" and data[8:12] == b"WEBP" and len(data) >= 30:
        chunk = data[12:16]
        if chunk == b"VP8X":
            return 1 + int.from_bytes(data[24:27], "little"), 1 + int.from_bytes(data[27:30], "little")
        if chunk == b"VP8L":
            bits = int.from_bytes(data[21:25], "little")
            return 1 + (bits & 0x3FFF), 1 + ((bits >> 14) & 0x3FFF)
        if chunk == b"VP8 ":
            width, height = struct.unpack("<HH", data[26:30])
            return width & 0x3FFF, height & 0x3FFF
    return None


def measure(result) -> dict:
    """The cost of one result as a model reads it: its content blocks."""
    text_bytes = 0
    images = []
    blocks = result.get("content", []) if isinstance(result, dict) else []
    if not blocks and isinstance(result, dict):
        # Not a tool result (tools/list, initialize): all of it is read.
        text_bytes = len(json.dumps(result, separators=(",", ":")).encode())
    for block in blocks:
        if block.get("type") == "text":
            text_bytes += len(block.get("text", "").encode())
        elif block.get("type") == "image":
            data = base64.b64decode(block.get("data", ""))
            width, height = image_size(data) or (0, 0)
            images.append({
                "mime": block.get("mimeType"),
                "width": width,
                "height": height,
                "bytes": len(data),
                "tokens": image_tokens(width, height),
            })
    tokens = text_tokens(text_bytes) + sum(image["tokens"] for image in images)
    return {"text_bytes": text_bytes, "images": images, "tokens": tokens}


def what(name: str, arguments: dict) -> tuple:
    """A short name for a call, and how many actions it ran."""
    if name == "tilo_invoke":
        calls = arguments.get("calls")
        if isinstance(calls, list):
            return "calls:" + ",".join(str(call.get("action")) for call in calls), len(calls)
        return str(arguments.get("action")), 1
    if name == "tilo_query":
        named = arguments.get("needle") or ("children" if arguments.get("parent_resource") else "root")
        return "query:" + named, 0
    if name == "tilo_resolve":
        return "resolve:" + str(arguments.get("action")), 0
    return name, 0


def summarise(records: list) -> dict:
    """Totals over one connection's records."""
    calls = [record for record in records if record.get("method") == "tools/call"]
    images = [image for record in records for image in record.get("images", [])]
    text_bytes = sum(record.get("text_bytes", 0) for record in records)
    image_tokens_total = sum(image["tokens"] for image in images)
    by_tool = {}
    for record in calls:
        by_tool[record["tool"]] = by_tool.get(record["tool"], 0) + 1
    started = min((record["start"] for record in records), default=0)
    ended = max((record["start"] + record["ms"] / 1000 for record in records), default=0)
    return {
        "requests": len(records),
        "tool_calls": len(calls),
        "actions": sum(record.get("actions", 0) for record in calls),
        "by_tool": by_tool,
        "text_bytes": text_bytes,
        "text_tokens": text_tokens(text_bytes),
        "images": len(images),
        "image_bytes": sum(image["bytes"] for image in images),
        "image_tokens": image_tokens_total,
        "tokens": text_tokens(text_bytes) + image_tokens_total,
        "wire_bytes": sum(record.get("wire_bytes", 0) for record in records),
        "call_s": round(sum(record.get("ms", 0) for record in records) / 1000, 1),
        "wall_s": round(ended - started, 1),
    }


def format_summary(summary: dict) -> str:
    return (
        f"{summary['tool_calls']} tool calls ({summary['actions']} actions), {summary['images']} images\n"
        f"  text    {summary['text_bytes']:>11,} B   ~{summary['text_tokens']:>7,} tokens\n"
        f"  images  {summary['image_bytes']:>11,} B   ~{summary['image_tokens']:>7,} tokens\n"
        f"  total   {summary['wire_bytes']:>11,} B on the wire   ~{summary['tokens']:>7,} tokens\n"
        f"  time    {summary['call_s']} s in calls, {summary['wall_s']} s wall"
    )


def read_log(path) -> list:
    records = []
    for line in path.read_text().splitlines():
        try:
            records.append(json.loads(line))
        except json.JSONDecodeError:
            continue
    return records

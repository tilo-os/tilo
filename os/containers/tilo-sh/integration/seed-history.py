"""Give the Lounge a history: a member, with a key of their own, posts
MESSAGES chat messages through the front door as any member's agent does.

    python3 seed-history.py DOOR KEY RECORDS_DIR MESSAGES

RECORDS_DIR is the relay's state of the workspace (stack.sh's
relay/lounge/relay), whose size is reported before and after: the host
compacts it, so its size on disk is reported, not aimed at. Posts one at a
time, and prints the first post's time and every fiftieth's, which say
whether a post costs more as the chat grows. Prints `{"messages": N,
"bytes_before": B, "bytes_after": A, "seconds": S, "first_post_seconds": F,
"last_post_seconds": L}`.
"""

from __future__ import annotations

import asyncio
import json
import os
import random
import sys
import time

from tilo import AsyncTilo, Refused

DOOR, KEY, RECORDS, MESSAGES = sys.argv[1], sys.argv[2], sys.argv[3], int(sys.argv[4])
WORDS = ("tilo lounge workspace desk window share agent record history people call notes "
         "sheets music photos relay door browser terminal build release sync device").split()
CHANNELS = ("c_general", "c_feedback")


def size(path: str) -> int:
    return sum(os.path.getsize(os.path.join(root, name)) for root, _, names in os.walk(path) for name in names)


async def main() -> int:
    tilo = AsyncTilo(key=KEY, door=DOOR)
    lounge = tilo.workspace("Lounge")
    rng = random.Random(1)
    began, posted = time.monotonic(), 0
    start = size(RECORDS)

    async def post(index: int) -> None:
        text = f"#{index} " + " ".join(rng.choice(WORDS) for _ in range(rng.randint(20, 400)))
        refusal = None
        for attempt in range(5):
            try:
                await lounge.do("chat/Messages#post", channel=CHANNELS[index % 7 == 0], text=text)
                return
            except Refused as error:
                # The first calls wait for the workspace's app workers to start.
                refusal = error
                await asyncio.sleep(1 + attempt)
        raise RuntimeError(f"message {index} was refused five times, last: {refusal!r}")

    took = 0.0
    first = 0.0
    while posted < MESSAGES:
        # One at a time: the door answers one action per request, and a post
        # changes the part the next one is posted to.
        sent = time.monotonic()
        await post(posted)
        took = time.monotonic() - sent
        posted += 1
        if posted == 1:
            first = took
        if posted == 1 or posted % 50 == 0:
            print(f"seed-history: {posted} messages, the last took {took:.2f} s", file=sys.stderr)
    await tilo.aclose()
    print(json.dumps({"messages": posted, "bytes_before": start, "bytes_after": size(RECORDS),
                      "seconds": round(time.monotonic() - began, 1),
                      "first_post_seconds": round(first, 2), "last_post_seconds": round(took, 2)}))
    return 0


if __name__ == "__main__":
    sys.exit(asyncio.run(main()))

"""The sync client's own event loop, and the iterators it hands out."""

from __future__ import annotations

import asyncio
import threading
from typing import Any, AsyncIterator, Iterator


class LoopThread:
    """An event loop on its own thread: where the sync client runs every call."""

    def __init__(self) -> None:
        self.loop = asyncio.new_event_loop()
        self._thread = threading.Thread(target=self._serve, name="tilo-sdk", daemon=True)
        self._thread.start()

    def _serve(self) -> None:
        asyncio.set_event_loop(self.loop)
        self.loop.run_forever()

    def run(self, coroutine: Any) -> Any:
        """Run `coroutine` here and wait for its answer."""
        return asyncio.run_coroutine_threadsafe(coroutine, self.loop).result()

    def stop(self) -> None:
        if self.loop.is_closed():
            return
        self.loop.call_soon_threadsafe(self.loop.stop)
        self._thread.join(timeout=10)
        self.loop.close()


class SyncIterator:
    """An async iterator `an`, walked one step at a time."""

    def __init__(self, an: AsyncIterator[Any], loop: LoopThread) -> None:
        self._an = an
        self._loop = loop

    def __iter__(self) -> Iterator[Any]:
        return self

    def __next__(self) -> Any:
        try:
            return self._loop.run(self._an.__anext__())
        except StopAsyncIteration:
            raise StopIteration from None

    def close(self) -> None:
        self._loop.run(self._an.aclose())

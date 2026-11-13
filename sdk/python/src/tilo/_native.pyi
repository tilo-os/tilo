"""The SDK's native part: Tilo's API in process, as a key the program holds.

Built from ``sdk/python/native`` into every wheel; absent when ``tilo`` runs
from a checkout that did not build it.
"""

from collections.abc import AsyncIterator
from typing import Any

class Host:
    """A Tilo host reached directly with the key a key file holds."""

    def __init__(self, key_file: str | dict[str, Any]) -> None:
        """``key_file``: the path of a key file, or its contents
        (``{"host", "name", "key", "cert"}``)."""

    async def request(
        self, method: str, path: str, headers: dict[str, str], body: bytes
    ) -> tuple[int, dict[str, str], bytes | AsyncIterator[bytes]]:
        """Answer ``method`` on ``path`` (with its query) as the API does: the
        body is ``bytes``, or an async iterator of chunks for a
        ``text/event-stream`` answer."""

"""The SDK's native part: this computer's daemon, reached over its native endpoint.

Built from ``sdk/python/native`` into every wheel; absent when ``tilo`` runs
from a checkout that did not build it, in which case a key over HTTPS is the
way to a workspace.
"""

class Daemon:
    """A connection to this computer's daemon, admitted as whoever makes it."""

    @property
    def workspace(self) -> str | None:
        """The workspace this connection acts in when a call names none."""

    async def request(
        self,
        method: str,
        target: str,
        headers: list[list[str]],
        body: bytes | None = None,
    ) -> Reply:
        """Have the daemon answer ``method`` on ``target`` (a ``/v1`` path with
        its query) with ``headers`` and ``body``, as its API answers this
        connection: an app with its own grants, never its person's.

        Awaits the ``Reply``. Raises ``ConnectionError`` when the daemon did not
        answer at all; a refusal is a reply, with its status.
        """

    async def web_call(self, workspace: str | None, args: str) -> str:
        """Make the web call ``args`` (JSON text) names with the key of the vault
        item it names, in ``workspace`` (``None``: the one this connection acts
        in).

        Answers JSON text: ``{"answer": ...}``, or
        ``{"refused": {"error": {...}}}``. An app's own connection has the
        vault add the key to the request; the secret never reaches the app.
        """

    async def release(self, workspace: str | None, item: str) -> str:
        """Release the secret of the vault item ``item`` in ``workspace``
        (``None``: the one this connection acts in) to this process.

        Answers JSON text: ``{"answer": {"secret": ...}}``, or
        ``{"refused": {"error": {...}}}``.
        """

    async def find(self, workspace: str | None, app: str, purpose: str) -> str:
        """The reference of the vault item tagged for ``app`` and ``purpose`` in
        ``workspace`` (``None``: the one this connection acts in), found on this
        app's own connection.

        Answers JSON text: ``{"answer": {"reference": ...}}`` or
        ``{"answer": {}}``, or ``{"refused": {"error": {...}}}``. It releases
        nothing.
        """

class Reply:
    """What the daemon's API answered one request with."""

    @property
    def status(self) -> int:
        """The status it answered with."""

    @property
    def headers(self) -> list[list[str]]:
        """Each header as ``[name, value]``."""

    async def next(self) -> bytes | None:
        """The next piece of the body; ``None`` once it ended.

        Raises ``ConnectionError`` when the connection closed first.
        """

async def connect(socket: str | None = None, workspace: str | None = None) -> Daemon:
    """Connect to the daemon's native endpoint.

    The connection the launcher handed an app it started comes first, then
    ``socket``, then this computer's own daemon. ``workspace`` names the one
    workspace the connection acts in when it knows it. Raises
    ``ConnectionError`` when no daemon answers.
    """

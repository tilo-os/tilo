"""The SDK's native part: this computer's daemon, reached in its action frames.

Built from ``sdk/python/native`` into every wheel; absent when ``tilo`` runs
from a checkout that did not build it, in which case a key over HTTPS is the
way to a workspace.
"""

class Daemon:
    """A connection to this computer's daemon, admitted as whoever makes it."""

    @property
    def workspace(self) -> str | None:
        """The workspace this connection acts in when a call names none."""

    async def call(
        self,
        workspace: str | None,
        tool: str,
        args: str,
        group: str | None = None,
    ) -> str:
        """Call ``tool`` with ``args`` (JSON text) in ``workspace`` (``None``: the
        account's own tree), as a step of ``group`` (JSON text of ``{id, title}``)
        when one is named.

        Answers JSON text: ``{"answer": ...}`` for a call that ran, and
        ``{"refused": {"error": {...}}}`` for one that was refused.
        """

async def connect(socket: str | None = None, workspace: str | None = None) -> Daemon:
    """Connect to the daemon's native endpoint.

    The connection the launcher handed an app it started comes first, then
    ``socket``, then this computer's own daemon. ``workspace`` names the one
    workspace the connection acts in when it knows it. Raises
    ``ConnectionError`` when no daemon answers.
    """

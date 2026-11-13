"""A visa startup's backend, one agent per applicant, end to end.

The backend holds one server key (`TILO_API_KEY`, all workspaces, no
expiry) and sends the API's calls to it (`TILO_API_URL`). It makes a
workspace per applicant, puts an applicant's passport and application form
into theirs and mints a key that reaches that workspace alone. The
applicant's agent, with only that key, fills the form in PDF Viewer and writes
a cover letter in Writer. The backend then invites the startup's ops person,
another person, as a reviewer of Jane's workspace: they follow the link, sign
in, and with a key of their own read what the agent did, who did it, and take
it back.

    TILO_API_URL=https://api.tilo.sh TILO_API_KEY=tilo_sk_… python3 visa_proof.py

The run prints the reviewer's invitation, then the links to the form and the
letter the agent changed, which open them in their apps for the reviewer, and
asks for the key the reviewer made after following the invitation; `TILO_INVITEE` instead names an endpoint that plays the
reviewer (the `api-on-relay` harness): it is posted the link and answers
the reviewer's key.

`VISA_APPLICANTS` sets how many workspaces it makes (200). With
`VISA_REVOKE_SERVER_KEY=1` it ends by revoking the server key itself, which
ends every key it minted and every session open with them: only for a key
made for this run.
"""

from __future__ import annotations

import asyncio
import io
import json
import os
import statistics
import struct
import sys
import time
import urllib.request
import zipfile
import zlib
from typing import Any, Mapping

from tilo import AsyncTilo, ProtocolError, Refused
from tilo.types.pdf_viewer import Document, FormFieldRow

APPLICANTS = int(os.environ.get("VISA_APPLICANTS", "200"))
REVOKE_SERVER_KEY = os.environ.get("VISA_REVOKE_SERVER_KEY") == "1"
RUN_TITLE = "Fill Jane Doe's visa application"
REVIEWER_EMAIL = "rosa@example.com"
FORM = "pdf-viewer/application.pdf"

FIRST = ["Jane", "Amara", "Luis", "Mei", "Omar", "Priya", "Tomas", "Yuki", "Noah", "Zara"]
LAST = ["Doe", "Okafor", "Garcia", "Chen", "Haddad", "Rao", "Novak", "Sato", "Berg", "Ali"]

FILLED = {
    "surname": {"kind": "text", "value": "Doe"},
    "given_names": {"kind": "text", "value": "Jane"},
    "passport_number": {"kind": "text", "value": "X1234567"},
    "purpose": {"kind": "choice", "value": "Business"},
    "declaration": {"kind": "checked", "value": {"on": True, "export": "Yes"}},
}

LETTER = [
    "Dear Visa Officer,",
    "I apply for a short-stay business visa to attend meetings with our partners in Berlin.",
    "My passport, the completed application form and my itinerary are enclosed.",
    "Yours faithfully, Jane Doe",
]


def say(step: str, detail: str = "") -> None:
    print(f"[{time.strftime('%H:%M:%S')}] {step}" + (f": {detail}" if detail else ""), flush=True)


def check(condition: Any, what: str) -> None:
    if not condition:
        raise AssertionError(what)
    print(f"    ok  {what}", flush=True)


def quantile(values: list[float], q: float) -> float:
    ordered = sorted(values)
    return ordered[round((len(ordered) - 1) * q)]


# -- the applicant's documents ----------------------------------------------


def passport_png() -> bytes:
    """A small picture standing in for a passport scan."""
    width, height = 64, 40
    rows = b"".join(
        b"\x00" + b"".join(bytes((40 + x * 3, 60 + y * 4, 140)) for x in range(width))
        for y in range(height)
    )

    def chunk(kind: bytes, data: bytes) -> bytes:
        return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", zlib.crc32(kind + data))

    header = struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0)
    return b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", header) + chunk(b"IDAT", zlib.compress(rows)) + chunk(b"IEND", b"")


def application_pdf() -> bytes:
    """A visa application with an AcroForm: three text fields, a choice and a checkbox."""

    def stream(dictionary: str, content: str) -> str:
        return f"{dictionary[:-2].rstrip()} /Length {len(content)} >>\nstream\n{content}\nendstream"

    widget = "/DA (/Helv 12 Tf 0 g) /F 4 /P 3 0 R"
    objects = [
        "<< /Type /Catalog /Pages 2 0 R /AcroForm << /Fields [4 0 R 5 0 R 6 0 R 7 0 R 8 0 R] "
        "/DA (/Helv 0 Tf 0 g) /DR << /Font << /Helv 11 0 R >> >> >> >>",
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 595 842] /Annots [4 0 R 5 0 R 6 0 R 7 0 R 8 0 R] "
        "/Contents 12 0 R /Resources << /Font << /Helv 11 0 R >> >> >>",
        f"<< /Type /Annot /Subtype /Widget /FT /Tx /T (surname) /V () /Rect [200 760 520 780] {widget} >>",
        f"<< /Type /Annot /Subtype /Widget /FT /Tx /T (given_names) /V () /Rect [200 720 520 740] {widget} >>",
        f"<< /Type /Annot /Subtype /Widget /FT /Tx /T (passport_number) /V () /Rect [200 680 520 700] {widget} >>",
        "<< /Type /Annot /Subtype /Widget /FT /Ch /Ff 131072 /T (purpose) /Opt [(Tourism) (Business) "
        f"(Visiting family)] /V (Tourism) /Rect [200 640 400 660] {widget} >>",
        "<< /Type /Annot /Subtype /Widget /FT /Btn /T (declaration) /V /Off /AS /Off /Rect [200 600 216 616] "
        "/F 4 /P 3 0 R /AP << /N << /Yes 9 0 R /Off 10 0 R >> >> >>",
        stream("<< /Type /XObject /Subtype /Form /BBox [0 0 16 16] >>", "0 g 3 3 10 10 re f"),
        stream("<< /Type /XObject /Subtype /Form /BBox [0 0 16 16] >>", "0 G 0.5 0.5 15 15 re S"),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>",
        stream(
            "<< >>",
            "BT /Helv 12 Tf 40 800 Td (Application for Schengen Visa) Tj 0 -36 Td (Surname) Tj "
            "0 -40 Td (Given names) Tj 0 -40 Td (Passport number) Tj 0 -40 Td (Purpose of journey) Tj "
            "0 -40 Td (I declare the above is true) Tj ET",
        ),
    ]
    out = bytearray(b"%PDF-1.7\n")
    offsets = []
    for number, body in enumerate(objects, 1):
        offsets.append(len(out))
        out += f"{number} 0 obj\n{body}\nendobj\n".encode("latin-1")
    xref = len(out)
    out += f"xref\n0 {len(objects) + 1}\n0000000000 65535 f \n".encode()
    for offset in offsets:
        out += f"{offset:010} 00000 n \n".encode()
    out += f"trailer\n<< /Size {len(objects) + 1} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n".encode()
    return bytes(out)


# -- reading answers ----------------------------------------------------------


def content_in(value: Any) -> str | None:
    """The first `tilo.content:` reference anywhere in an answer."""
    if isinstance(value, str):
        return value if value.startswith("tilo.content:") else None
    if isinstance(value, Mapping):
        value = list(value.values())
    if isinstance(value, list):
        for item in value:
            found = content_in(item)
            if found:
                return found
    return None


def values_of(fields: list[FormFieldRow]) -> dict[str, Any]:
    return {field["name"]: field["value"] for field in fields}


async def refused(call: Any) -> str:
    """How the API refused `call`: an HTTP status, or a refusal's code."""
    try:
        await call
    except ProtocolError as error:
        return str(error.status or error.code or error.message)
    except Refused as error:
        return f"{error.code}: {error.message}"
    return "allowed"


def beyond(refusal: str) -> bool:
    """Whether `refusal` is the API saying the key does not reach that far."""
    return any(refusal.startswith(code) for code in ("beyond_scope", "forbidden", "not_permitted"))


async def reviewer_accepts(link: str) -> dict[str, Any]:
    """The reviewer follows `link` and makes a key of their own: `{key, person, joined}`."""
    invitee = os.environ.get("TILO_INVITEE")
    if not invitee:
        print(f"Reviewer: open {link}, sign in, then create a key.", flush=True)
        key = (await asyncio.to_thread(input, "The reviewer's key: ")).strip()
        return {"key": key, "person": None, "joined": {"role": "editor"}}

    def post() -> dict[str, Any]:
        request = urllib.request.Request(
            invitee,
            data=json.dumps({"link": link}).encode(),
            headers={"content-type": "application/json"},
        )
        with urllib.request.urlopen(request, timeout=120) as answer:
            return json.load(answer)

    return await asyncio.to_thread(post)


# -- the proof ----------------------------------------------------------------


async def make_workspaces(backend: AsyncTilo) -> list[Any]:
    say(f"1. The backend makes {APPLICANTS} applicant workspaces with its one server key")
    made, timings = [], []
    for index in range(APPLICANTS):
        name = f"Applicant: {FIRST[index % 10]} {LAST[index // 10 % 10]}"
        if index >= 100:
            name += f" ({index // 100 + 1})"
        began = time.perf_counter()
        made.append(await backend.workspaces.create(name))
        timings.append(time.perf_counter() - began)
    began = time.perf_counter()
    listing = await backend.workspaces.list()
    listed = time.perf_counter() - began
    ids = {workspace.id for workspace in listing}
    p50, p95 = quantile(timings, 0.5), quantile(timings, 0.95)
    say(
        "   timings",
        f"create p50 {p50 * 1000:.0f} ms, p95 {p95 * 1000:.0f} ms, max {max(timings) * 1000:.0f} ms, "
        f"mean {statistics.mean(timings) * 1000:.0f} ms; list of {len(ids)} in {listed * 1000:.0f} ms",
    )
    check(len({workspace.id for workspace in made}) == APPLICANTS, f"{APPLICANTS} distinct workspaces, no cap")
    check(all(workspace.id in ids for workspace in made), "every one of them is listed")
    check(p95 < 2.0, f"making one stays fast (p95 {p95 * 1000:.0f} ms < 2 s)")
    check(listed < 5.0, f"listing them stays fast ({listed * 1000:.0f} ms < 5 s)")
    return made


async def main() -> int:
    url = os.environ.get("TILO_API_URL", "https://api.tilo.sh")
    say("API", url)
    backend = AsyncTilo()

    made = await make_workspaces(backend)
    jane, other = made[0], made[1]
    check(jane.name == "Applicant: Jane Doe", f"the first is {jane.name!r}")

    say("2. The backend puts Jane's documents into her workspace and mints her agent's key")
    ws = backend.workspace(jane.id)
    passport_bytes, form_bytes = passport_png(), application_pdf()
    passport = await ws.files.upload(passport_bytes)
    application = await ws.files.upload(form_bytes)
    say("   uploaded", f"passport.png {passport}, application.pdf {application}")
    check(passport.startswith("tilo.content:") and application.startswith("tilo.content:"), "both are content")
    minted = await ws.keys.mint(name="Jane Doe's agent")
    say("   minted", f"{minted.name!r} for {minted.workspace}, mcp_url {minted.mcp_url}")
    check(minted.workspace == jane.id, "the key names Jane's workspace")
    check(minted.mcp_url.endswith(f"/w/{jane.id}/mcp"), "its MCP URL is that workspace's")
    check(minted.token and minted.token != backend.transport.key, "it is a key of its own")

    say("3. Jane's agent, with only the minted key, fills the form and writes the letter")
    agent = AsyncTilo(key=minted.token, url=url)
    aws = agent.workspace(jane.id)
    check(await aws.files.download(passport) == passport_bytes, "the agent reads the passport the backend put there")
    async with aws.run(RUN_TITLE) as run:
        await aws.do("pdf-viewer#open")
        imported = await aws.do("pdf-viewer#import-pdf-content", content=application, name="application.pdf")
        say("   imported", str(imported)[:200])
        form = Document(aws, FORM)
        await form.load()
        before = (await form.read_form_fields())["fields"]
        say("   fields", ", ".join(f"{field['name']} ({field['kind']})" for field in before))
        check({field["name"] for field in before} >= set(FILLED), "the form has every field the agent fills")
        for field in before:
            if field["name"] in FILLED:
                await aws.do(
                    f"{FORM}#field-edit",
                    edit={"path": field["path"], "op": "set", "value": FILLED[field["name"]]},
                )
        filled = values_of((await form.read_form_fields())["fields"])
        say("   filled", str(filled))
        check(all(filled[name] == value for name, value in FILLED.items()), "every field holds what the agent set")

        await aws.do("word-processor#open")
        for paragraph in LETTER:
            await aws.do("word-processor#append-paragraph", text=paragraph)
        exported = await aws.do("word-processor#export-content", format="docx")
        letter_ref = content_in(exported)
        check(letter_ref, f"Writer exports the letter as content ({letter_ref})")
    check(run.runs, f"the run grouped the agent's {len(run.runs)} calls")
    form_link = await aws.link(FORM)
    say("   links", ", ".join(run.links))
    check(form_link in run.links, f"the run hands over the link to the form it filled ({form_link})")
    letter = await ws.files.download(letter_ref)
    check(letter.startswith(b"PK"), f"the backend downloads a Word file ({len(letter)} bytes)")
    with zipfile.ZipFile(io.BytesIO(letter)) as docx:
        body = docx.read("word/document.xml").decode("utf-8")
    check("Dear Visa Officer" in body and "Jane Doe" in body, "the letter holds what the agent wrote")

    say("4a. Scope: the minted key reaches Jane's workspace and nothing else")
    elsewhere = await refused(agent.workspace(other.id).ls())
    check(elsewhere != "allowed", f"another applicant's workspace refuses it ({elsewhere})")
    upload_elsewhere = await refused(agent.workspace(other.id).files.upload(b"x"))
    check(upload_elsewhere != "allowed", f"it puts nothing into another applicant's workspace ({upload_elsewhere})")
    listed = await agent.workspaces.list()
    say("   listed", ", ".join(workspace.name for workspace in listed))
    check(
        [workspace.id for workspace in listed] == [jane.id],
        f"the account lists Jane's workspace alone ({[workspace.name for workspace in listed]})",
    )
    creating = await refused(agent.workspaces.create("Applicant: Mallory"))
    check(beyond(creating), f"the account refuses it: no new workspace ({creating})")
    minting = await refused(agent.keys.mint(jane.id, name="escalated"))
    check(beyond(minting), f"it mints no keys ({minting})")

    say("4b. The backend invites its ops person, another person, as Jane's reviewer")
    invitation = await ws.members.invite(role="editor", email=REVIEWER_EMAIL)
    say("   invited", f"{invitation.role} via {invitation.link[:60]}… sent={invitation.sent} ({invitation.next})")
    check(invitation.link.startswith("http"), "the invitation is a link to hand over")
    check(invitation.email == REVIEWER_EMAIL and invitation.sent is False, "it names their address and says no mail went")
    say("   for review", f"the form: {form_link}")
    accepted = await reviewer_accepts(invitation.link)
    say("   accepted", str(accepted.get("joined")))
    check((accepted.get("joined") or {}).get("role") == "editor", "the link lands them in Jane's workspace as an editor")
    reviewer_person = accepted.get("person")
    members = await ws.members.list()
    say("   members", ", ".join(f"{member.name or member.person[:8]} ({member.role})" for member in members))
    check(
        reviewer_person is None or any(member.person == reviewer_person and member.role == "editor" for member in members),
        "the backend sees the reviewer among Jane's workspace's members",
    )
    check(
        all(member.name != "Jane Doe's agent" for member in members),
        "no key of the account's names a person in the members list",
    )
    reviewer = AsyncTilo(key=accepted["key"], url=url)

    say("4c. The reviewer, with their own key, finds Jane's workspace by name and reads the filled form and who filled it")
    listed = await reviewer.workspaces.list()
    found = listed.named(jane.name)
    check(found is not None and found.id == jane.id, f"the reviewer's account lists {jane.name!r}: {found}")
    check(found is not None and found.role == "editor", f"with the role they hold there ({found and found.role})")
    rws = reviewer.workspace(jane.name)
    seen = Document(rws, FORM)
    await seen.load()
    say("   read", f"{seen.kind}: {sorted(seen.fields)}")
    reviewed = values_of((await seen.read_form_fields())["fields"])
    check(
        all(reviewed[name] == value for name, value in FILLED.items()),
        f"the reviewer sees the filled values: {reviewed}",
    )
    history = await rws.do(f"{FORM}#history", last=50)
    versions = [version for version in history.get("versions", []) if isinstance(version, Mapping)]
    for version in versions:
        say("   history", str({key: version.get(key) for key in ("sentence", "by", "run", "run_id", "actor", "undone") if key in version}))
    by_agent = [version for version in versions if (version.get("actor") or {}).get("kind") == "agent"]
    check(by_agent, f"{len(by_agent)} of {len(versions)} changes are the agent's")
    names = {(version.get("actor") or {}).get("name") for version in by_agent}
    check(names == {"Jane Doe's agent"}, f"History names the agent by its key's name ({names})")
    colours = {(version.get("actor") or {}).get("colour") for version in by_agent}
    check(len(colours) == 1 and None not in colours, f"in one colour ({colours})")
    people = {(version.get("actor") or {}).get("for") for version in by_agent}
    check(len(people) == 1 and None not in people, f"each on behalf of the one person ({people})")
    runs = {version["run_id"]: version.get("run") for version in by_agent if version.get("run_id")}
    say("   runs", str(runs))
    check(list(runs.values()) == [RUN_TITLE], f"History names the agent's one run {RUN_TITLE!r}")
    check(reviewer_person not in people, "the reviewer is not the person the agent acted for")

    say("4d. The reviewer undoes the agent's run")
    run_id = next(iter(runs))
    await rws.do(f"{FORM}#undo", run=run_id)
    back = values_of((await seen.read_form_fields())["fields"])
    say("   after undo", str(back))
    check(back["surname"] == {"kind": "text", "value": ""}, "the surname is empty again")
    check(back["purpose"] == {"kind": "choice", "value": "Tourism"}, "the purpose is back to what the form said")

    say("4e. No VM ran for any of it")
    usage = await backend.usage()
    jane_usage = next((row for row in usage.workspaces if row.workspace == jane.id), None)
    say("   usage", f"host {usage.host!r}, plan {usage.plan!r}, {len(usage.workspaces)} workspaces; Jane's: {jane_usage}")
    check(jane_usage is not None, "the account meters Jane's workspace")
    check(jane_usage.vm_seconds == 0 and not jane_usage.running, "no VM time in Jane's workspace, none running")
    check(usage.vm_seconds == 0 and not usage.running, f"0 VM seconds across all {len(usage.workspaces)} workspaces")
    check(usage.raw.get("standing", {}).get("account") in people, "the account is the person the agent acted for")

    say("4f. Revoking ends what it reached")
    await backend.keys.revoke(minted.grant)
    gone = await refused(aws.ls())
    check(gone in ("401", "403"), f"the agent's open session ends with its key ({gone})")
    if REVOKE_SERVER_KEY:
        second = await ws.keys.mint(name="Jane Doe's second agent")
        sws = AsyncTilo(key=second.token, url=url).workspace(jane.id)
        await sws.ls()
        own = (await backend.keys.list())[0]
        await backend.keys.revoke(own.grant)
        check(await refused(backend.workspaces.list()) == "401", "the server key is refused")
        check(await refused(sws.ls()) == "401", "the session of a key it minted ended with it")
        fresh = AsyncTilo(key=second.token, url=url).workspace(jane.id)
        check(await refused(fresh.ls()) == "401", "the key it minted is refused")

    for client in (agent, reviewer, backend):
        try:
            await client.aclose()
        except (ProtocolError, Refused):
            pass
    say("All of it held")
    return 0


if __name__ == "__main__":
    sys.exit(asyncio.run(main()))

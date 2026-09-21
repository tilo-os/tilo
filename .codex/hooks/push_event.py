"""Decode the supported push tool shapes without executing shell or JavaScript."""

from __future__ import annotations

import json
import re
import shlex
from pathlib import Path, PureWindowsPath

MAX_RESPONSE_DEPTH = 4
MAX_RESPONSE_CHARS = 1_000_000


def completed_successfully(response: object) -> bool:
    """Accept a terminal result or Codex's completed output-only exec envelope.

    ``text(result.output)`` deliberately omits the inner exit code.  The outer
    envelope still proves that the exec cell completed; ``watch_ci.main`` then
    uses the remote branch's exact SHA as the authoritative push-success check.
    """
    results = []
    wrapper_completed = False

    def collect(value: object, depth: int = 0) -> bool:
        nonlocal wrapper_completed
        if depth > MAX_RESPONSE_DEPTH:
            return False
        if isinstance(value, str):
            if len(value) > MAX_RESPONSE_CHARS:
                return False
            try:
                return collect(json.loads(value), depth + 1)
            except ValueError:
                if value.startswith("Script completed\n"):
                    wrapper_completed = True
                return True
        if isinstance(value, list):
            return bool(value) and all(collect(item, depth + 1) for item in value)
        if not isinstance(value, dict):
            return False
        if value.get("isError") or value.get("is_error") or value.get("session_id") or value.get("cell_id"):
            return False
        if "exit_code" in value or "exitCode" in value:
            code = value.get("exit_code", value.get("exitCode"))
            if type(code) is not int or code != 0:
                return False
            results.append(value)
            return True
        if value == {}:  # A literal apply_patch result preceding the command.
            return True
        if value.get("type") in ("text", "input_text") and isinstance(value.get("text"), str):
            return collect(value["text"], depth + 1)
        content = value.get("content", value.get("output"))
        return isinstance(content, list) and collect(content, depth + 1)

    valid = collect(response)
    return valid and (len(results) == 1 or (not results and wrapper_completed))


def literal_call(code: str) -> dict | None:
    """Accept one literal exec_command call and its ordinary output wrapper only.

    Deliberately reject variables, interpolation, control flow and multiple calls.
    Parsing arbitrary JavaScript would turn a notification hook into an interpreter.
    """
    code = code.strip()
    if code.startswith("// @exec:"):
        code = code.partition("\n")[2].strip()
    # A common observed turn applies a literal patch before its one shell call.
    # Consume that inert JSON string; never search its contents for commands.
    prefix = "text(await tools.apply_patch("
    while code.startswith(prefix):
        try:
            value, consumed = json.JSONDecoder().raw_decode(code[len(prefix):])
        except ValueError:
            return None
        rest = code[len(prefix) + consumed:].lstrip()
        if not isinstance(value, str) or not rest.startswith("));"):
            return None
        code = rest[3:].strip()
    match = re.fullmatch(
        r"(?:text\(\s*await\s+|await\s+|(?:const|let)\s+(?P<var>\w+)\s*=\s*await\s+)"
        r"tools\.exec_command\(\s*(?P<object>\{.*\})\s*\)(?P<tail>.*)",
        code,
        re.DOTALL,
    )
    if not match:
        return None
    tail = match["tail"].strip()
    variable = match["var"]
    if variable:
        if tail != ";" and not re.fullmatch(
            rf";\s*text\(\s*{re.escape(variable)}(?:\.output)?\s*\)\s*;?", tail
        ):
            return None
    elif code.startswith("text("):
        if tail not in (")", ");"):
            return None
    elif tail not in ("", ";"):
        return None

    # JSON values only; permit JavaScript's unquoted property names.
    source = match["object"]
    decoder = json.JSONDecoder()
    result: dict = {}
    cursor = 1
    while True:
        cursor += len(source[cursor:]) - len(source[cursor:].lstrip())
        if source[cursor:] == "}":
            return result
        key_match = re.match(r'(?P<key>"(?:[^"\\]|\\.)*"|[A-Za-z_]\w*)\s*:\s*', source[cursor:])
        if not key_match:
            return None
        key = key_match["key"]
        key = json.loads(key) if key.startswith('"') else key
        if key in result:
            return None
        cursor += key_match.end()
        try:
            value, consumed = decoder.raw_decode(source[cursor:])
        except ValueError:
            return None
        if not isinstance(value, (str, int, float, bool, type(None))):
            return None
        result[key] = value
        cursor += consumed
        cursor += len(source[cursor:]) - len(source[cursor:].lstrip())
        if source[cursor:] == "}":
            return result
        if source[cursor:cursor + 1] != ",":
            return None
        cursor += 1


def push_arguments(command: str) -> list[str] | None:
    # These can change either execution or repository identity. No interpretation.
    if any(char in command for char in ("`", "$", "\n", "\r")):
        return None
    try:
        lexer = shlex.shlex(command, posix=True, punctuation_chars=";&|")
        lexer.whitespace_split = True
        lexer.commenters = ""
        tokens = list(lexer)
    except ValueError:
        return None
    segments: list[list[str]] = [[]]
    for token in tokens:
        if token in (";", "&&", "||"):
            segments.append([])
        elif token in ("&", "|", "|&"):
            return None
        else:
            segments[-1].append(token)
    pushes = []
    for words in segments:
        if not words:
            continue
        if words[0].lower() in ("cd", "chdir", "set-location", "pushd", "popd"):
            return None
        if words[0] == "git":
            if len(words) < 2 or words[1].startswith("-"):
                return None
            if words[1] in ("checkout", "switch", "reset", "worktree"):
                return None
            if words[1] == "push":
                pushes.append(words[2:])
    return pushes[0] if len(pushes) == 1 else None


def push_target(event: dict) -> tuple[Path, list[str]] | None:
    tool = event.get("tool_name", "").removeprefix("functions.")
    data = event.get("tool_input")
    if tool == "exec":
        code = data.get("code") if isinstance(data, dict) else data
        data = literal_call(code) if isinstance(code, str) else None
        tool = "exec_command"
    if not isinstance(data, dict) or tool not in ("Bash", "exec_command"):
        return None
    command = data.get("command" if tool == "Bash" else "cmd")
    cwd = data.get("workdir", event.get("cwd"))
    if not isinstance(command, str) or not isinstance(cwd, str) or not (Path(cwd).is_absolute() or PureWindowsPath(cwd).is_absolute()):
        return None
    if not completed_successfully(event.get("tool_response")):
        return None
    arguments = push_arguments(command)
    return (Path(cwd), arguments) if arguments is not None else None


def pushes_current_branch(arguments: list[str], branch: str) -> bool:
    """Only normal origin pushes of this HEAD; no delete, mirror or other ref."""
    args = list(arguments)
    if args[:1] in (["-u"], ["--set-upstream"]):
        args.pop(0)
    return args in (
        ["origin", branch], ["origin", "HEAD"],
        ["origin", f"HEAD:refs/heads/{branch}"],
        ["origin", f"HEAD:{branch}"],
    )

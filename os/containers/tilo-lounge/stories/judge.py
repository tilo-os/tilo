"""A first-time user's verdict on a recorded user story.

The judge is a model that knows nothing of Tilo's code: it is given the
story's intent in plain words, the steps with how long each took as a
stopwatch would show it, and the recording itself (each step's screenshots and
frames from each person's video). It says whether anything was slow,
confusing, ugly, broken-looking or wrong, as a picky person trying Tilo for the
first time would, and whether it accepts the story.

usage: python3 -I judge.py STORY_DIR
Reads STORY_DIR/story.json and the recording beside it, writes
STORY_DIR/judge.json. Needs ANTHROPIC_API_KEY; without it the verdict says the
story was not judged, which never counts as accepted.
"""

import base64
import json
import os
import pathlib
import subprocess
import sys
import tempfile

MODEL = "claude-opus-5-5"
# The screenshots and frames one verdict looks at: enough to follow every
# step, few enough that each is looked at properly.
MOST_PICTURES = 40
FRAMES_PER_VIDEO = 10

SYSTEM = """You are trying a new product called Tilo for the first time. You are \
picky: you notice anything slow, confusing, ugly, broken-looking or wrong, and \
you have no patience for a product that makes you wait or guess. You know \
nothing about how Tilo is built and you do not care; you judge only what a \
person would see and feel.

You are shown a recording of people doing one thing with Tilo: what they set \
out to do, the steps they took with how long each took, and screenshots and \
video frames of their screens. Judge it as one of those people.

Reject the story if any of these holds:
- what they set out to do did not happen, or the recording stops before it did;
- a screen looks broken: blank or half-drawn areas, overlapping or cut-off text, \
error messages, placeholder or debug text, wrong or missing names;
- a step took longer than a person would accept for it;
- a person would not know what to do next or what just happened;
- it looks careless or ugly enough that you would not trust it with your work.

Accept it only if you would be happy to have done this yourself. Name each \
problem by the step it is in and say plainly what a person would notice."""

VERDICT = {
    "type": "object",
    "properties": {
        "accept": {"type": "boolean"},
        "summary": {"type": "string"},
        "problems": {
            "type": "array",
            "items": {
                "type": "object",
                "properties": {
                    "step": {"type": ["integer", "null"]},
                    "kind": {
                        "type": "string",
                        "enum": ["slow", "confusing", "ugly", "broken", "wrong"],
                    },
                    "severity": {"type": "string", "enum": ["blocker", "major", "minor"]},
                    "what": {"type": "string"},
                },
                "required": ["step", "kind", "severity", "what"],
                "additionalProperties": False,
            },
        },
    },
    "required": ["accept", "summary", "problems"],
    "additionalProperties": False,
}


def catalog_entry(story_id: str) -> dict:
    stories = json.loads((pathlib.Path(__file__).parent / "stories.json").read_text())["stories"]
    return next(entry for entry in stories if entry["id"] == story_id)


def picture(path: pathlib.Path, media_type: str) -> dict:
    data = base64.standard_b64encode(path.read_bytes()).decode()
    return {"type": "image", "source": {"type": "base64", "media_type": media_type, "data": data}}


def video_frames(video: pathlib.Path, seconds: float, into: pathlib.Path) -> list[pathlib.Path]:
    """Frames spread evenly over the video, as JPEGs."""
    every = max(seconds / FRAMES_PER_VIDEO, 1.0)
    subprocess.run(
        ["ffmpeg", "-loglevel", "error", "-i", str(video), "-vf", f"fps=1/{every:.2f}",
         "-frames:v", str(FRAMES_PER_VIDEO), "-q:v", "4", str(into / f"{video.stem}-%02d.jpg")],
        check=False,
    )
    return sorted(into.glob(f"{video.stem}-*.jpg"))


def recording(story_dir: pathlib.Path, story: dict, entry: dict, scratch: pathlib.Path) -> list[dict]:
    """The intent, the steps as timed, and the pictures, as one message."""
    steps = story["steps"]
    lines = [
        f"What they set out to do: {entry['intent']}",
        "",
        "The steps, with how long each took:",
    ]
    for step in steps:
        lines.append(f"{step['index']}. {step['title']}: {step['ms'] / 1000:.1f} s")
    planned = entry.get("steps", [])
    if len(steps) < len(planned):
        lines.append(f"The recording stops here; the steps not reached: {', '.join(planned[len(steps):])}.")
    content: list[dict] = [{"type": "text", "text": "\n".join(lines)}]
    pictures = 0
    for step in steps:
        for shot in step.get("shots", []):
            if pictures >= MOST_PICTURES - FRAMES_PER_VIDEO * len(story.get("videos", [])):
                break
            content.append({"type": "text", "text": f"Step {step['index']} ({step['title']}), as it ended, on {shot['person']}'s screen:"})
            content.append(picture(story_dir / shot["file"], "image/jpeg" if shot["file"].endswith(".jpg") else "image/png"))
            pictures += 1
    for video in story.get("videos", []):
        frames = video_frames(story_dir / video["file"], story.get("seconds", 60), scratch)
        if frames:
            content.append({"type": "text", "text": f"Frames from {video['person']}'s screen, in order, over the whole story:"})
            content.extend(picture(frame, "image/jpeg") for frame in frames)
    content.append({"type": "text", "text": "Your verdict, as that first-time user."})
    return content


def judge(story_dir: pathlib.Path) -> dict:
    story = json.loads((story_dir / "story.json").read_text())
    entry = catalog_entry(story["id"])
    # A story nobody sees, measured only (such as files copied through a mounted folder), gives
    # the model nothing to look at: its numbers against their budgets are all it has.
    if not story.get("videos") and not any(step.get("shots") for step in story.get("steps", [])):
        return {"judged": False, "applicable": False, "accept": True, "model": MODEL,
                "summary": "Nothing on screen to judge: this story is measured, not watched.", "problems": []}
    if not os.environ.get("ANTHROPIC_API_KEY"):
        return {"judged": False, "accept": False, "model": MODEL,
                "summary": "Not judged: this run had no ANTHROPIC_API_KEY.", "problems": []}
    import anthropic

    client = anthropic.Anthropic()
    with tempfile.TemporaryDirectory() as scratch:
        content = recording(story_dir, story, entry, pathlib.Path(scratch))
        try:
            response = client.beta.messages.create(
                model=MODEL,
                max_tokens=16000,
                system=SYSTEM,
                messages=[{"role": "user", "content": content}],
                output_config={"effort": "high", "format": {"type": "json_schema", "schema": VERDICT}},
                # A request the model's safety classifiers decline is answered
                # by the model they route it to instead.
                betas=["server-side-fallback-2026-07-01"],
                fallbacks="default",
            )
        except anthropic.APIStatusError as error:
            return {"judged": False, "accept": False, "model": MODEL,
                    "summary": f"Not judged: the API answered {error.status_code}: {error.message}", "problems": []}
        except anthropic.APIConnectionError as error:
            return {"judged": False, "accept": False, "model": MODEL,
                    "summary": f"Not judged: the API could not be reached: {error}", "problems": []}
    if response.stop_reason == "refusal":
        return {"judged": False, "accept": False, "model": response.model,
                "summary": "Not judged: the model declined to answer.", "problems": []}
    text = "".join(block.text for block in response.content if block.type == "text")
    verdict = json.loads(text)
    return {"judged": True, "model": response.model, **verdict}


def main() -> int:
    if len(sys.argv) != 2:
        print("usage: python3 -I judge.py STORY_DIR", file=sys.stderr)
        return 2
    story_dir = pathlib.Path(sys.argv[1])
    verdict = judge(story_dir)
    (story_dir / "judge.json").write_text(json.dumps(verdict, indent=2))
    print(f"JUDGE {'not needed' if verdict.get('applicable') is False else 'accepts' if verdict['accept'] else 'rejects'}: {verdict['summary']}")
    for problem in verdict.get("problems", []):
        print(f"  step {problem['step']}: {problem['severity']} {problem['kind']}: {problem['what']}")
    return 0


if __name__ == "__main__":
    sys.exit(main())

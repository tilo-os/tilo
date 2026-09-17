from pathlib import Path
import hashlib, io, json, subprocess, sys, tarfile
repo = Path(__file__).resolve().parents[2]
run_name = sys.argv[1]
if not run_name or Path(run_name).name != run_name:
    raise SystemExit("Use a single run-directory name")
out = Path(__file__).resolve().parent / run_name
out.mkdir(exist_ok=False)
raw = subprocess.check_output(["git", "ls-files", "-z", "--cached", "--others", "--exclude-standard", "--", ".", ":(exclude).superpowers/**", ":(exclude)target/**"], cwd=repo)
paths = sorted(set(x.decode() for x in raw.split(b"\0") if x))
hashes = {}
with tarfile.open(out / "linux-source.tar.gz", "w:gz") as archive:
    for relative in paths:
        source = repo / relative
        if not source.is_file():
            continue
        if source.is_symlink():
            raise SystemExit(f"Review source symlink before snapshot: {relative}")
        data = source.read_bytes()
        hashes[relative] = hashlib.sha256(data).hexdigest()
        info = archive.gettarinfo(str(source), arcname=relative)
        info.size = len(data)
        archive.addfile(info, io.BytesIO(data))
(out / "linux-source-hashes.json").write_text(json.dumps(hashes, indent=2) + "\n")
(out / "source-head.txt").write_bytes(subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=repo))
(out / "working-diff.patch").write_bytes(subprocess.check_output(["git", "diff", "--binary"], cwd=repo))
print(f"Snapshot {len(hashes)} source files to {out}")

from pathlib import Path
import concurrent.futures,hashlib,json,os,subprocess,tomllib,time
repo=Path(__file__).resolve().parents[2]
work=Path(__file__).resolve().parent
run_name=os.environ.get("QOS_REVIEW_RUN", "after"); run=work/run_name;run.mkdir(exist_ok=True)
if (run/"capture-results.json").exists():
    raise SystemExit("Capture run already has evidence; choose a fresh QOS_REVIEW_RUN")
capture=run/"captures";capture.mkdir(exist_ok=True)
apps=json.loads((work/"inventory.json").read_text())
selected=set(filter(None,os.environ.get("QOS_REVIEW_APPS", "").split(",")))
if selected:apps=[a for a in apps if Path(a["directory"]).name in selected]
source_head=subprocess.check_output(["git","rev-parse","HEAD"],cwd=repo).decode().strip()
source_diff=subprocess.check_output(["git","diff","--binary"],cwd=repo)
(run/"working-diff.patch").write_bytes(source_diff)
variants=[("wide","1200x800",1),("compact","800x700",1),("narrow","480x900",1),("large-text","800x900",2),("scale-baseline","800x900",1)]
if os.environ.get("QOS_REVIEW_SHORT") == "1":
    variants.append(("short","800x480",1))
selected_variants=set(filter(None,os.environ.get("QOS_REVIEW_VARIANTS", "").split(",")))
if selected_variants:
    unknown=selected_variants-{v[0] for v in variants}
    if unknown:
        raise SystemExit("Unknown variants: "+", ".join(sorted(unknown)))
    variants=[v for v in variants if v[0] in selected_variants]
if not apps:
    raise SystemExit("No matching apps selected")
def one(app):
    directory=app["directory"]; name=Path(directory).name
    out=capture/name;out.mkdir(exist_ok=True)
    if name=="agent":return dict(app=directory,status="windowless",captures=[])
    if name in ["windows","osk"]:return dict(app=directory,status="platform-build-blocked",captures=[])
    manifest=tomllib.loads((repo/directory/"Cargo.toml").read_text())
    bins=manifest.get("bin",[])
    binary_name=next((b["name"] for b in bins if b.get("path")=="src/main.rs"), bins[0]["name"] if bins else app["package"])
    binary=repo/"target/debug"/binary_name
    if not binary.exists():return dict(app=directory,status="missing-binary",binary=str(binary),captures=[])
    digest=hashlib.sha256(binary.read_bytes()).hexdigest(); results=[]
    for variant,viewport,scale in variants:
        png=out/(variant+".png");profile=out/(variant+"-profile");profile.mkdir(exist_ok=True)
        command=[str(binary),"--shots",str(png),"--viewport",viewport]
        if scale!=1:command += ["--text-scale",str(scale)]
        env=dict(os.environ,HOME=str(profile),XDG_DATA_HOME=str(profile/"data"),XDG_CONFIG_HOME=str(profile/"config"),XDG_CACHE_HOME=str(profile/"cache"),XDG_STATE_HOME=str(profile/"state"),QUICK_UI_VELLO_CPU="1",QOS_SHOTS_THEME="dark",QOS_SHOTS_TEXT_SCALE=str(scale))
        start=time.monotonic(); png_sha256=None; actual=None
        try:
            with (out/(variant+".log")).open("w") as log:
                result=subprocess.run(command,cwd=repo,env=env,stdout=log,stderr=subprocess.STDOUT,timeout=90)
            status="captured" if result.returncode==0 and png.is_file() else "failed"
            if status=="captured":
                import struct
                data=png.read_bytes()
                if len(data)<24 or data[:8]!=b"\x89PNG\r\n\x1a\n":
                    status="invalid-png"
                else:
                    actual=struct.unpack(">II",data[16:24])
                    png_sha256=hashlib.sha256(data).hexdigest()
                requested=tuple(map(int,viewport.split("x")))
                if status=="captured" and actual!=requested:status="dimension-mismatch"
            code=result.returncode
        except subprocess.TimeoutExpired:status="timeout";code=None
        results.append(dict(variant=variant,command=command,status=status,exit_code=code,seconds=round(time.monotonic()-start,2),path=str(png),png_sha256=png_sha256,actual_dimensions=actual,requested_scale=scale))
        print(name,variant,status,flush=True)
    final_digest=hashlib.sha256(binary.read_bytes()).hexdigest()
    binary_stable=final_digest==digest
    if not binary_stable:
        print(name,"binary changed during capture; fresh capture required",flush=True)
    result=dict(app=directory,status="captured" if binary_stable and all(x["status"]=="captured" for x in results) else "partial",binary=str(binary),binary_sha256=digest,binary_sha256_after=final_digest,binary_stable=binary_stable,source_head=source_head,working_diff_sha256=hashlib.sha256(source_diff).hexdigest(),captures=results)
    (out/"manifest.json").write_text(json.dumps(result,indent=2)+"\n")
    return result
results=[]
with concurrent.futures.ThreadPoolExecutor(max_workers=3) as pool:
    for result in pool.map(one,apps):
        results.append(result)
        (run/"capture-results.json").write_text(json.dumps(results,indent=2)+"\n")
print("Capture batch complete",flush=True)

if any(r["status"] not in ("captured","windowless","platform-build-blocked") for r in results):
    raise SystemExit(1)

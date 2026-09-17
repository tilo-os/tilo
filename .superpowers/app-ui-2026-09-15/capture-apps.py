from pathlib import Path
import concurrent.futures,hashlib,json,os,subprocess,tomllib,time
repo=Path(__file__).resolve().parents[2]
work=Path(__file__).resolve().parent
capture=work/"captures";capture.mkdir(exist_ok=True)
apps=json.loads((work/"inventory.json").read_text())
source_head=subprocess.check_output(["git","rev-parse","HEAD"],cwd=repo).decode().strip()
source_diff=subprocess.check_output(["git","diff","--binary"],cwd=repo)
(work/"baseline-working-diff.patch").write_bytes(source_diff)
variants=[("wide","1200x800",1),("compact","800x700",1),("narrow","480x900",1),("large-text","800x900",2)]
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
        env=dict(os.environ,HOME=str(profile),XDG_DATA_HOME=str(profile/"data"),XDG_CONFIG_HOME=str(profile/"config"),XDG_CACHE_HOME=str(profile/"cache"),QUICK_UI_VELLO_CPU="1",QOS_SHOTS_THEME="dark",QOS_SHOTS_TEXT_SCALE=str(scale))
        start=time.monotonic()
        try:
            with (out/(variant+".log")).open("w") as log:
                result=subprocess.run(command,cwd=repo,env=env,stdout=log,stderr=subprocess.STDOUT,timeout=90)
            status="captured" if result.returncode==0 and png.is_file() else "failed"
            code=result.returncode
        except subprocess.TimeoutExpired:status="timeout";code=None
        results.append(dict(variant=variant,command=command,status=status,exit_code=code,seconds=round(time.monotonic()-start,2),path=str(png),requested_scale=scale))
        print(name,variant,status,flush=True)
    result=dict(app=directory,status="captured" if all(x["status"]=="captured" for x in results) else "partial",binary=str(binary),binary_sha256=digest,source_head=source_head,working_diff_sha256=hashlib.sha256(source_diff).hexdigest(),captures=results)
    (out/"manifest.json").write_text(json.dumps(result,indent=2)+"\n")
    return result
results=[]
with concurrent.futures.ThreadPoolExecutor(max_workers=3) as pool:
    for result in pool.map(one,apps):
        results.append(result)
        (work/"capture-results.json").write_text(json.dumps(results,indent=2)+"\n")
print("Capture batch complete",flush=True)

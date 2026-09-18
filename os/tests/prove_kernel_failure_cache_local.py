import pathlib,tempfile,subprocess,time,json,uuid,argparse,hashlib
parser=argparse.ArgumentParser(description="Offline rootful Podman/BuildKit failed-RUN cache proof; requires pinned images already cached")
parser.add_argument("--output",type=pathlib.Path,required=True)
args=parser.parse_args();args.output.mkdir(parents=True,exist_ok=True)
base=args.output;image='docker.io/moby/buildkit@sha256:5a8cd84cb3fcfd082789a08f92bd36f8e745c6231edd78e24a3bf34fd471a823';busybox='docker.io/library/busybox@sha256:f10e809bcf667d8e9f01d2baf82869049a495cd448cdfe1f4dee94078b960ae9'
name='tilo-kernel-cache-proof-'+uuid.uuid4().hex[:10];cid=None;receipt={}
def run(args,**kwargs):return subprocess.run(args,capture_output=True,timeout=kwargs.pop('timeout',90),**kwargs)
with tempfile.TemporaryDirectory(prefix='tilo-failure-cache-proof-',dir='/var/tmp') as tmp:
 root=pathlib.Path(tmp);context=root/'context';context.mkdir();store=root/'store';store.mkdir()
 mounted=run(['podman','image','mount',busybox]);assert mounted.returncode==0
 try:
  copied=run(['cp','-a',mounted.stdout.decode().strip(),str(context/'rootfs')]);assert copied.returncode==0,copied.stderr.decode()
  assert (context/'rootfs/bin/busybox').stat().st_nlink==406
 finally:run(['podman','image','unmount',busybox])
 (context/'Dockerfile').write_text('''FROM scratch AS base
COPY rootfs/ /
SHELL ["/bin/busybox","sh","-c"]
FROM base AS failed
ARG KERNEL_CHECKPOINT_RUN_ID
ARG KERNEL_CHECKPOINT_ATTEMPT
RUN --mount=type=cache,id=tilo-kernel-failure-${KERNEL_CHECKPOINT_RUN_ID}-${KERNEL_CHECKPOINT_ATTEMPT},target=/cache,sharing=locked \\
    printf 'once\\n' >>/cache/count && printf 'public-object\\n' >/cache/object && printf "33e83f64cde9df4ee65ff47a708f125d932256da:%s:%s" "$KERNEL_CHECKPOINT_RUN_ID" "$KERNEL_CHECKPOINT_ATTEMPT" >/cache/ready && exit 23
FROM base AS exporter
ARG KERNEL_CHECKPOINT_RUN_ID
ARG KERNEL_CHECKPOINT_ATTEMPT
RUN --mount=type=cache,id=tilo-kernel-failure-${KERNEL_CHECKPOINT_RUN_ID}-${KERNEL_CHECKPOINT_ATTEMPT},target=/cache,sharing=locked,ro \\
    test "$(/bin/busybox cat /cache/ready)" = "33e83f64cde9df4ee65ff47a708f125d932256da:${KERNEL_CHECKPOINT_RUN_ID}:${KERNEL_CHECKPOINT_ATTEMPT}" && test "$(/bin/busybox wc -l </cache/count)" = 1 && /bin/busybox mkdir /result && /bin/busybox cp /cache/object /cache/count /result/
FROM scratch AS public
COPY --from=exporter /result/ /
''')
 try:
  p=run(['podman','run','-d','--name',name,'--cpus=1','--memory=768m','--pids-limit=128','--privileged','--security-opt=seccomp=unconfined','--security-opt=label=disable','-v',str(context)+':/context:rw','-v',str(store)+':/var/lib/buildkit:rw',image,'--oci-worker-snapshotter=native','--oci-worker-net=host','--containerd-worker=false'])
  assert p.returncode==0,p.stderr.decode()[-2000:];cid=p.stdout.decode().strip()
  for _ in range(15):
   ready=run(['podman','exec',cid,'buildctl','debug','workers'],timeout=5)
   if ready.returncode==0:break
   time.sleep(.2)
  assert ready.returncode==0,'BuildKit did not start'
  common=['podman','exec',cid,'buildctl','build','--frontend=dockerfile.v0','--local','context=/context','--local','dockerfile=/context','--opt','build-arg:KERNEL_CHECKPOINT_RUN_ID=12345','--opt','build-arg:KERNEL_CHECKPOINT_ATTEMPT=2','--progress=plain']
  first=run(common+['--opt','target=failed'],timeout=90)
  (base/'kernel-failure-buildkit-first.log').write_bytes(first.stdout+first.stderr)
  assert first.returncode!=0 and b'exit code: 23' in first.stderr,first.stderr.decode()[-3000:]
  second=run(common+['--opt','target=public','--output','type=local,dest=/context/result'],timeout=90)
  (base/'kernel-failure-buildkit-export.log').write_bytes(second.stdout+second.stderr)
  assert second.returncode==0,second.stderr.decode()[-3000:]
  assert (context/'result/count').read_bytes()==b'once\n' and (context/'result/object').read_bytes()==b'public-object\n'
  receipt={'schema':1,'purpose':'live-buildkit-failed-run-cache-proof','buildkit_image':image,'base':busybox,'cache_identity':'tilo-kernel-failure-12345-2','source_run_attempt_stamp_verified':True,'hardlinks_preserved':True,'script_sha256':hashlib.sha256(pathlib.Path(__file__).read_bytes()).hexdigest(),'make_like_exit_status':23,'export_did_not_rerun_compile':True,'public_bytes_match':True,'real_kernel_compiled':False,'scope':'Actual BuildKit failure cache mount and independent export target; capture helper separately unit tested'}
 finally:
  if cid:
   stop=run(['podman','rm','-f',cid]);assert stop.returncode==0,stop.stderr.decode()
receipt['owned_daemon_and_cache_removed']=True;(base/'kernel-failure-buildkit-proof.json').write_text(json.dumps(receipt,indent=2));print(json.dumps(receipt))

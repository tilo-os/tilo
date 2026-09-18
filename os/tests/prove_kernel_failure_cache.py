#!/usr/bin/env python3
"""Tiny live BuildKit proof; no source/kernel/software compilation or image pull."""
import argparse,json,pathlib,re,subprocess,tempfile,uuid
p=argparse.ArgumentParser();p.add_argument('--base',required=True);p.add_argument('--output',type=pathlib.Path,required=True);a=p.parse_args()
assert re.fullmatch(r'(?:sha256:[0-9a-f]{64}|[a-zA-Z0-9./_-]+@sha256:[0-9a-f]{64})',a.base)
cache='tilo-failed-run-fixture-'+uuid.uuid4().hex
with tempfile.TemporaryDirectory(prefix='tilo-failed-run-fixture-') as temporary:
 root=pathlib.Path(temporary);recipe=root/'Containerfile'
 recipe.write_text('''# syntax=docker/dockerfile:1.7
ARG BASE_REF
FROM ${BASE_REF} AS failed
RUN --mount=type=cache,id='''+cache+''',target=/fixture,sharing=locked \\
    printf 'once\\n' >> /fixture/count && printf 'public-object\\n' >/fixture/object && exit 23
FROM ${BASE_REF} AS export
RUN --mount=type=cache,id='''+cache+''',target=/fixture,sharing=locked,ro \\
    test "$(wc -l </fixture/count)" = 1 && mkdir /result && cp /fixture/object /fixture/count /result/
FROM scratch AS public
COPY --from=export /result/ /
FROM ${BASE_REF} AS cleanup
RUN --mount=type=cache,id='''+cache+''',target=/fixture,sharing=locked \\
    rm -f /fixture/count /fixture/object && test -z "$(ls -A /fixture)"
''')
 prefix=['docker','buildx','build','--pull=false','--progress=plain','--file',str(recipe),'--build-arg','BASE_REF='+a.base]
 first=subprocess.run(prefix+['--target','failed',str(root)],stdout=subprocess.PIPE,stderr=subprocess.PIPE,timeout=90)
 assert first.returncode!=0 and b'exit code: 23' in first.stderr,'fake failure did not preserve exit23'
 try:
  second=subprocess.run(prefix+['--target','public','--output','type=local,dest='+str(root/'out'),str(root)],stdout=subprocess.PIPE,stderr=subprocess.PIPE,timeout=90)
  assert second.returncode==0,second.stderr.decode()[-2000:]
  assert (root/'out/count').read_bytes()==b'once\n' and (root/'out/object').read_bytes()==b'public-object\n'
 finally:
  subprocess.run(prefix+['--target','cleanup',str(root)],stdout=subprocess.PIPE,stderr=subprocess.PIPE,timeout=90,check=True)
 result={'purpose':'tiny-buildkit-failed-run-cache-proof','base':a.base,'cache_id':cache,'failed_compile_exit':23,'export_did_not_rerun_compile':True,'public_bytes_match':True,'owned_cache_files_removed':True,'software_compiled':False}
 a.output.write_text(json.dumps(result,indent=2)+'\n');print(json.dumps(result))

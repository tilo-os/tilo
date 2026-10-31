#!/usr/bin/env python3
"""Talk to the trial desktop's agent door the way an MCP client does.

    TRIAL_SSH_CONFIG=ssh_config python3 mcp_probe.py [CLIENT_NAME] ['[["tool", {args}], ...]']

Runs `tilo-runtime-mcp --connect` in the VM over ssh (host `trial-vm` in the
ssh config), initializes as CLIENT_NAME (default claude-code), lists the tools
and calls each given tool in turn. A first connection by a new client name
waits until someone answers the admission prompt on the desktop.
"""
import json, os, subprocess, sys, threading, time
p = subprocess.Popen(["ssh", "-F", os.environ.get("TRIAL_SSH_CONFIG", "ssh_config"), "-T", "trial-vm", "tilo-runtime-mcp --connect"],
                     stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, bufsize=1)
def err():
    for l in p.stderr: print("ERR", l.rstrip()[:300], flush=True)
threading.Thread(target=err, daemon=True).start()
def send(o): p.stdin.write(json.dumps(o) + "\n"); p.stdin.flush()
def recv(want):
    while True:
        l = p.stdout.readline()
        if not l: print("EOF"); sys.exit(1)
        m = json.loads(l)
        if m.get("id") == want: return m
        print("NOTE", l[:200], flush=True)
send({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":sys.argv[1] if len(sys.argv)>1 else "claude-code","version":"2.0"}}})
t=time.time(); r = recv(1); print("init after %.1fs:" % (time.time()-t), json.dumps(r)[:400], flush=True)
send({"jsonrpc":"2.0","method":"notifications/initialized"})
send({"jsonrpc":"2.0","id":2,"method":"tools/list"})
r = recv(2); print("tools:", [t["name"] for t in r.get("result",{}).get("tools",[])], json.dumps(r.get("error"))[:300], flush=True)
for i, (name, args) in enumerate(json.loads(sys.argv[2]) if len(sys.argv)>2 else []):
    send({"jsonrpc":"2.0","id":10+i,"method":"tools/call","params":{"name":name,"arguments":args}})
    r = recv(10+i); print(name, json.dumps(r)[:3000], flush=True)
p.stdin.close(); p.wait(5)

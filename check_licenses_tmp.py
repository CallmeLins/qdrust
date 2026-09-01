import re, os, subprocess, json
ALLOW = {"0BSD","Apache-2.0","BSD-2-Clause","BSD-3-Clause","CDLA-Permissive-2.0","ISC","MIT","Unicode-3.0","Zlib"}
out = subprocess.run(["cargo","metadata","--format-version=1"], capture_output=True, text=True)
data = json.loads(out.stdout)
def resolve(name):
    for p in data["packages"]:
        if p["name"]==name: return p["id"]
    return None
root = resolve("qdrust-plugin-browser")
from collections import deque
reached=set(); q=deque([root])
while q:
    pid=q.popleft()
    if pid in reached: continue
    reached.add(pid)
    for p in data["packages"]:
        if p["id"]==pid:
            for d in p["dependencies"]: q.append(resolve(d["name"]))
            break
problems=[]
for p in data["packages"]:
    if p["id"] in reached:
        lic=p.get("license") or ""
        tokens=[t.strip().strip('()') for t in re.split(r'\s+(?:OR|AND)\s+',lic) if t.strip().strip('()')]
        if not tokens:
            problems.append((p["name"],p["version"],"(unknown/license-file)",lic))
        else:
            if not any(t in ALLOW for t in tokens):
                problems.append((p["name"],p["version"],", ".join(tokens),lic))
print(f"Reached packages: {len(reached)}")
if problems:
    print("POTENTIAL LICENSE PROBLEMS:")
    for n,v,l,f in sorted(problems): print(f"  {n} {v}: {l}  (full: {f})")
else:
    print("No obvious license problems.")

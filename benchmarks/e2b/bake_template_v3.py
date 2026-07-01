#!/usr/bin/env python3
"""Re-bake → `semfs-baked-v3` = semfs-baked-v2 + office-format WRITER libs.

Why: WB-Lite synthesis cases (45 .docx, 55 .doc, 386/388 .pptx) require the agent to
PRODUCE office files. semfs's extracted index covers READING the inputs, but WRITING the
graded deliverable needs python-docx / python-pptx, which were NOT in v2. The sandbox has
no network egress at runtime, so the agent's pip-install loops fail → 45-min timeout
(case 45 trace: 40 calls all `pip install python-docx` → timeout, 0 deliverable). openpyxl
WAS baked (.xlsx case 171 completed), so only docx/pptx are missing. Bake them at build
time (build infra HAS network).

Inherits v2 (corpus + q4/clean/leanhint3/4arm seeds) — NO copy context needed.
Run:  python3 benchmarks/e2b/bake_template_v3.py
Then: set WB_E2B_TEMPLATE=semfs-baked-v3 for re-runs.
"""
from e2b import Template

t = Template()
b = (
    t.from_template("semfs-baked-v2")
     # Diagnosed via a live v2 sandbox: the agent's python is /usr/bin/python3, which has NO pip,
     # NO ensurepip, and NO openpyxl. The .xlsx cases worked because the agent HAND-BUILT the OOXML
     # zip with stdlib `zipfile`; the .docx cases timed out because the agent chose `pip install
     # python-docx` and there's no pip (network IS fine — pypi=200). Fix: install pip (apt, as root,
     # build-time network), then the writer libs into /usr/bin/python3 so `import docx/pptx` just works.
     .set_user("root")
     .run_cmd("apt-get update -qq && apt-get install -y -qq python3-pip")
     .run_cmd("python3 -m pip install --break-system-packages --no-cache-dir python-docx python-pptx openpyxl")
     .run_cmd("python3 -c 'import docx, pptx, openpyxl; print(chr(111)+chr(107))'")
     .set_user("user")
)
print("building 'semfs-baked-v3' (v2 + python-docx + python-pptx + openpyxl) …")
info = Template.build(b, name="semfs-baked-v3", cpu_count=4, memory_mb=8192, request_timeout=1800.0,
                      on_build_logs=lambda e: print("  >", getattr(e, "message", str(e))[:160]))
print("TEMPLATE BUILT:", info)

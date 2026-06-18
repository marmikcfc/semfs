#!/usr/bin/env python3
"""Re-bake the semfs E2B template → `semfs-baked-v2`.

Inherits the live `semfs-baked` (gemma embedder + WB harness + cases + fuse deps + the
adaptive-K binary) via from_template, then:
  - ADD the plain-arm raw corpus at /opt/corpus      → plain arm boots 0-upload (was 442MB/boot)
  - REFRESH the seed at /opt/chanpin-gemma-q4.db      → current decontaminated index + dense KG
                                                         (Leiden+kNN, 32 communities vs old 173)
The semfs binary stays the per-boot push (run_matrix pushes semfs-fixed, 37MB ~15s); refresh that
separately (Modal build_semfs.py) when the sufficiency arm is needed.

Context dir (CTX) must contain:  corpus.tgz  (the exact run_matrix tarball)  and  chanpin-gemma-q4.db
Run:  python3 benchmarks/e2b/bake_template_v2.py
"""
from e2b import Template

CTX = "/tmp/e2b_ctx"

t = Template(file_context_path=CTX)
b = (
    t.from_template("semfs-baked")
     .copy("corpus.tgz", "/opt/corpus.tgz", user="root")                    # plain tarball (extract in-sandbox, no upload)
     .copy("chanpin-gemma-q4.db", "/opt/chanpin-gemma-q4.db", user="root")  # refresh seed (dense KG + decontaminated)
)

# request_timeout bumped to 1800s: the default 60s times out on the 442MB corpus + 690MB seed
# single-PUT context uploads (each context file is loaded whole and PUT in one httpx request).
print("building 'semfs-baked-v2' (semfs-baked + /opt/corpus.tgz + dense seed) @ 4cpu/8192mb, 1800s upload timeout …")
info = Template.build(b, name="semfs-baked-v2", cpu_count=4, memory_mb=8192, request_timeout=1800.0,
                      on_build_logs=lambda e: print("  >", getattr(e, "message", str(e))[:160]))
print("TEMPLATE BUILT:", info)

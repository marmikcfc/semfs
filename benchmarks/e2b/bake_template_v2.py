#!/usr/bin/env python3
"""Re-bake the semfs E2B template → `semfs-baked-v2`.

Inherits the live `semfs-baked` (gemma embedder + WB harness + cases + fuse deps + the
adaptive-K binary) via from_template, then:
  - ADD the plain-arm raw corpus at /opt/corpus      → plain arm boots 0-upload (was 442MB/boot)
  - REFRESH the seed at /opt/chanpin-gemma-q4.db      → current decontaminated index + dense KG
                                                         (Leiden+kNN, 32 communities vs old 173)
  - ADD the clean benchmark seed at /opt/chanpin-clean.db
  - ADD the shipped v4.1 hint seed at /opt/chanpin-leanhint3.db
  - ADD the shared hidden-KG seed at /opt/chanpin-4arm.db
The semfs binary stays the per-boot push (run_matrix pushes semfs-fixed, 37MB ~15s); refresh that
separately (Modal build_semfs.py) when the sufficiency arm is needed.

Context dir (CTX) must contain:
  - corpus.tgz               (the exact run_matrix tarball)
  - chanpin-gemma-q4.db
  - chanpin-clean.db
  - chanpin-leanhint3.db
  - chanpin-4arm.db
Run:  python3 benchmarks/e2b/bake_template_v2.py
"""
from e2b import Template

CTX = "/tmp/e2b_ctx"

t = Template(file_context_path=CTX)
b = (
    t.from_template("semfs-baked")
     .copy("corpus.tgz", "/opt/corpus.tgz", user="root")                    # plain tarball (extract in-sandbox, no upload)
     .copy("chanpin-gemma-q4.db", "/opt/chanpin-gemma-q4.db", user="root")  # refresh seed (dense KG + decontaminated)
     .copy("chanpin-clean.db", "/opt/chanpin-clean.db", user="root")        # clean baseline / hiddenkg seed
     .copy("chanpin-leanhint3.db", "/opt/chanpin-leanhint3.db", user="root")# best_exp0002 seed (v4.1 hint)
     .copy("chanpin-4arm.db", "/opt/chanpin-4arm.db", user="root")          # shared hidden-KG experiment seed
)

# request_timeout bumped to 1800s: the default 60s times out on the corpus + multi-seed upload
# single-PUT context uploads (each context file is loaded whole and PUT in one httpx request).
print("building 'semfs-baked-v2' (semfs-baked + /opt/corpus.tgz + q4/clean/leanhint3/4arm seeds) @ 4cpu/8192mb, 1800s upload timeout …")
info = Template.build(b, name="semfs-baked-v2", cpu_count=4, memory_mb=8192, request_timeout=1800.0,
                      on_build_logs=lambda e: print("  >", getattr(e, "message", str(e))[:160]))
print("TEMPLATE BUILT:", info)

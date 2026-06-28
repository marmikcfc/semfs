"""Minimal Modal smoke test.

This intentionally avoids the semfs benchmark image, EC2 pull, OpenRouter,
Codex, and seeded data. It proves only the first required layer:
can this machine reach Modal, start a tiny function, and write/read a shared
Modal Volume?

Run:
  modal volume create semfs-bench-data  # harmless if it already exists
  modal run benchmarks/modal/modal_min_smoke.py::smoke
"""

from __future__ import annotations

import json
import os
import socket
import time
from pathlib import Path

import modal


app = modal.App("semfs-min-smoke")
volume = modal.Volume.from_name("semfs-bench-data", create_if_missing=True)

image = modal.Image.from_registry("python:3.11-slim")
MOUNT = "/data"


def _local_preflight() -> None:
    try:
        socket.getaddrinfo("api.modal.com", 443, proto=socket.IPPROTO_TCP)
    except OSError as exc:
        raise RuntimeError(
            "local shell cannot resolve api.modal.com; Modal CLI cannot run from this environment"
        ) from exc


@app.function(image=image, volumes={MOUNT: volume}, timeout=120)
def remote_smoke() -> dict:
    stamp = str(int(time.time()))
    path = Path(MOUNT) / "_smoke" / "modal_min_smoke.json"
    path.parent.mkdir(parents=True, exist_ok=True)
    payload = {
        "ok": True,
        "stamp": stamp,
        "cwd": os.getcwd(),
        "mount_exists": Path(MOUNT).exists(),
    }
    path.write_text(json.dumps(payload, sort_keys=True), encoding="utf-8")
    volume.commit()
    readback = json.loads(path.read_text(encoding="utf-8"))
    print(json.dumps(readback, indent=2))
    return readback


@app.local_entrypoint()
def smoke():
    _local_preflight()
    print(json.dumps(remote_smoke.remote()))

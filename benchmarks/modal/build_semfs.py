"""Build the fixed x86_64-linux `semfs` binary on Modal (matches the E2B sandbox arch).

Local cross-compile is impossible here (fastembed/ONNX + no docker), and the E2B
sandbox is small (4c/7G). Modal x86_64-linux with a big box builds it fast, writes
the binary to the volume, then we `modal volume get` it and push it into E2B at boot.

Usage:
  modal volume put semfs-bench-data /tmp/semfs_src.tgz /_build/semfs_src.tgz --force
  modal run benchmarks/modal/build_semfs.py
  modal volume get semfs-bench-data /bin/semfs-fixed benchmarks/e2b/assets/semfs-fixed --force
"""
import modal

app = modal.App("semfs-build")
vol = modal.Volume.from_name("semfs-bench-data")
VOL = "/data"
# Official rust image (cargo/rustc preinstalled) + the C/SSL toolchain semfs needs
# (rusqlite bundled + tree-sitter need cc; reqwest native-tls needs libssl).
# Mirror docker/Dockerfile.release exactly (the proven recipe): rust:1.95-slim +
# only pkg-config/libssl-dev. add_python is required by the Modal runtime. NO
# clang/cmake (they perturbed the linker in the prior attempt).
image = (
    modal.Image.from_registry("rust:1.95-slim", add_python="3.11")
    # g++ provides the libstdc++.so dev symlink that ONNX Runtime (C++, via fastembed)
    # needs at link time — `rust-lld: unable to find library -lstdc++` without it.
    .apt_install("git", "pkg-config", "libssl-dev", "g++")
)


@app.function(image=image, volumes={VOL: vol}, cpu=8.0, memory=32768, timeout=2400)
def build():
    import subprocess, os, shutil
    os.makedirs("/tmp/b", exist_ok=True)
    subprocess.run("tar xzf /data/_build/semfs_src.tgz -C /tmp/b", shell=True, check=True)
    print("== building semfs (release) ==", flush=True)
    os.makedirs(f"{VOL}/_build", exist_ok=True)
    # full verbose log → volume, so any error survives for `modal volume get`.
    r = subprocess.run(
        "cargo build --release --bin semfs --verbose > /data/_build/build.log 2>&1",
        shell=True, cwd="/tmp/b", text=True,
    )
    vol.commit()
    binp = "/tmp/b/target/release/semfs"
    if not os.path.exists(binp):
        tail = subprocess.run("tail -50 /data/_build/build.log", shell=True,
                              capture_output=True, text=True).stdout
        print("---- build.log (tail 50) ----")
        print(tail)
        raise SystemExit("BUILD FAILED — see /data/_build/build.log")
    os.makedirs(f"{VOL}/bin", exist_ok=True)
    shutil.copy(binp, f"{VOL}/bin/semfs-fixed")
    vol.commit()
    ver = subprocess.run([binp, "--version"], capture_output=True, text=True).stdout.strip()
    sz = os.path.getsize(binp)
    # confirm the timeout-env knobs are compiled in (string-grep the binary)
    have = subprocess.run(
        f"strings {binp} | grep -c -E 'SEMFS_SEARCH_TIMEOUT_SECS|SEMFS_SEARCH_DEADLINE_SECS|SEMFS_GREP_CLIENT_WAIT_SECS'",
        shell=True, capture_output=True, text=True).stdout.strip()
    print(f"OK: {ver}  ({sz/1e6:.1f} MB)  env-knob-strings-found={have}")
    return {"version": ver, "size": sz, "knobs": have}


@app.local_entrypoint()
def main():
    print(build.remote())

#!/usr/bin/env python3
"""Live-process USS/PSS/RSS driver.

Launches target.py under a chosen interpreter, waits for READY, then reads the
kernel's own accounting from /proc/<pid>/smaps_rollup (USS = Private_Clean +
Private_Dirty; PSS = proportional set size; RSS = resident). Optionally saves
the full /proc/<pid>/smaps for per-library attribution.

Usage: driver.py <target_python> <mode> [smaps_out_path]
Prints one JSON object on stdout.
"""
import json
import os
import subprocess
import sys
import time

ROLLUP_KEYS = ("Rss", "Pss", "Shared_Clean", "Shared_Dirty",
               "Private_Clean", "Private_Dirty", "Referenced", "Anonymous")


def read_rollup(pid):
    out = {}
    with open(f"/proc/{pid}/smaps_rollup") as f:
        for line in f:
            for k in ROLLUP_KEYS:
                if line.startswith(k + ":"):
                    out[k] = int(line.split()[1])  # KiB
    out["USS"] = out.get("Private_Clean", 0) + out.get("Private_Dirty", 0)
    return out


def main():
    target_python, mode = sys.argv[1], sys.argv[2]
    smaps_out = sys.argv[3] if len(sys.argv) > 3 else None
    here = os.path.dirname(os.path.abspath(__file__))
    target = os.path.join(here, "target.py")

    p = subprocess.Popen([target_python, target, mode],
                         stdout=subprocess.PIPE, text=True)
    pid = None
    deadline = time.time() + 120
    while time.time() < deadline:
        line = p.stdout.readline()
        if line.startswith("READY"):
            pid = int(line.split()[1])
            break
        if not line and p.poll() is not None:
            print(json.dumps({"error": "target died", "rc": p.returncode}))
            return
    if pid is None:
        p.kill()
        print(json.dumps({"error": "timeout waiting for READY"}))
        return

    # Let things settle a beat, then snapshot.
    time.sleep(0.3)
    rollup = read_rollup(pid)
    if smaps_out:
        with open(f"/proc/{pid}/smaps") as f, open(smaps_out, "w") as g:
            g.write(f.read())

    p.terminate()
    try:
        p.wait(timeout=5)
    except subprocess.TimeoutExpired:
        p.kill()

    rollup["mode"] = mode
    rollup["pid"] = pid
    print(json.dumps(rollup))


if __name__ == "__main__":
    main()

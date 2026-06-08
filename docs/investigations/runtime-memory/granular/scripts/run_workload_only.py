#!/usr/bin/env python3
"""Minimal entrypoint: import + run the realistic workload, nothing else.
Used as the target for `memray run --native`. cwd = sites/, bench env python."""
import os
import sys
HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import workload
s = workload.run_workload(rounds=int(os.environ.get("MB_ROUNDS", "3")))
sys.stderr.write(f"workload done: {s}\n")

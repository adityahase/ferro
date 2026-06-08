#!/usr/bin/env python3
"""Full realistic workload with optional lazy-import stubs, measuring final RSS
(+ post-trim). Used to confirm the import lever and allocator lever STACK.
env STUB="..."; allocator chosen by caller via LD_PRELOAD/MALLOC_*. cwd=sites/."""
import os
import sys
import types
import gc
import ctypes

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
STUB = [s for s in os.environ.get("STUB", "").split(",") if s]


class _Dummy:
    def __call__(self, *a, **k): return _Dummy()
    def __getattr__(self, n): return _Dummy()
    def __mro_entries__(self, bases): return (object,)
    def __iter__(self): return iter(())


class StubModule(types.ModuleType):
    __path__ = []
    def __getattr__(self, n):
        if n.startswith("__") and n.endswith("__"):
            raise AttributeError(n)
        return _Dummy()


class _StubLoader:
    def create_module(self, spec): return StubModule(spec.name)
    def exec_module(self, module): pass


class StubFinder:
    def find_spec(self, name, path=None, target=None):
        if name.split(".")[0] in STUB:
            import importlib.machinery
            return importlib.machinery.ModuleSpec(name, _StubLoader())
        return None


if STUB:
    sys.meta_path.insert(0, StubFinder())


def rollup():
    d = {}
    for l in open("/proc/self/smaps_rollup"):
        if l.startswith(("Rss:", "Anonymous:")):
            k, v = l.split(":")
            d[k.strip()] = int(v.split()[0])
    return d


def main():
    label = sys.argv[1] if len(sys.argv) > 1 else "?"
    import workload
    try:
        workload.run_workload(rounds=int(os.environ.get("MB_ROUNDS", "3")))
    except Exception as e:
        sys.stderr.write(f"workload error: {e}\n")
    gc.collect(); gc.collect()
    r0 = rollup()
    rss0, anon = r0.get("Rss", 0), r0.get("Anonymous", 0)
    rss1 = rss0
    try:
        libc = ctypes.CDLL("libc.so.6"); libc.malloc_trim(0)
        rss1 = rollup().get("Rss", 0)
    except Exception:
        pass
    print(f"{label}\tRSS_MB={rss0/1024:.2f}\tRSS_trim_MB={rss1/1024:.2f}"
          f"\tANON_MB={anon/1024:.2f}\tn_modules={len(sys.modules)}")


if __name__ == "__main__":
    main()

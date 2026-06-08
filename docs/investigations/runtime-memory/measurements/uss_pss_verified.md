# Raw USS/PSS/COW measurements (verified)

All values KiB unless noted. USS = Private_Clean + Private_Dirty. Source: kernel
`/proc/<pid>/smaps_rollup` on live blocked processes; cross-checked with `pmap -XX`.
Interpreter: CPython 3.14.4, bench env `/home/frappe/benches/bench-cpython314/env/bin/python`,
site `mysite.sqlite`. Host: aditya-cloud, Ubuntu 26.04, 2 vCPU, ~7.7 GiB RAM, kernel 7.0.0.

## Single-process ladder (primary, smaps_rollup)

| mode | Rss | Pss | USS | Shared_Clean | Private_Dirty |
|---|--:|--:|--:|--:|--:|
| bare | 10572 | 4662 | 3484 | 7088 | 3476 |
| bare_imports | 20476 | 11314 | 9480 | 10996 | 8192 |
| import_frappe | 60488 | 49950 | 47468 | 13020 | 41492 |
| init_connect | 62876 | 52282 | 49800 | 13076 | 43764 |
| warm_meta (×10) | 115820 | 105106 | 102592 | 13228 | 83904 |
| get_all | 111344 | 100631 | 98116 | 13228 | 79428 |

## 4 independent warm workers (current model), aggregate

| run | agg Rss | agg Pss | agg USS | per-worker USS | per-worker Shared_Clean |
|---|--:|--:|--:|--:|--:|
| primary    | 463272 | 360354 | 335640 | ~83.9 MB | ~31.9 MB |
| pmap -XX   | ~452900 | 350186 | 325124 | ~81.3 MB | ~31.9 MB |
| audit      | 452876 | 350052 | 325280 | ~81.3 MB | ~31.9 MB |

Mechanism: 1→4 workers, per-worker Shared_Clean 13228→~31900, Private_Clean ~18800→~0,
RSS flat ~113 MB, Private_Dirty pinned ~81–84 MB/worker (object graph, shared with no one).

## 1 parent + 4 forked children (gunicorn --preload / COW), aggregate family PSS

| workload | family PSS | per-child USS | ratio vs independent (~350 MB) |
|---|--:|--:|--:|
| idle children            | 107–111 MB | 0.8–1.5 MB | **~3.2–3.3×** |
| + gc.freeze()            | 107–111 MB | 0.8–1.5 MB | identical (freeze no help) |
| light single read/child  | 127–163 MB | 5–15 MB | ~2.2–2.8× |
| sustained realistic loop | 154–273 MB | 13–42 MB | ~1.3–2.2× |
| pathological clear_cache | 270–388 MB | 42–71 MB | ~0.9–1.3× (can INVERT) |

Shared_Dirty carries the COW heap: ~80 MB idle → erodes to ~60–72 MB under load as children
fault private copies. gc.freeze() before fork: 106770 vs 106705 (idle), 272747 vs 272091 (loop)
— negligible; erosion is refcount-write driven, not GC-traversal driven.

## Checks
- ru_maxrss (%M) is PEAK RSS, always ≥ live snapshot (demo: 91852 peak vs 10020 after free);
  agrees with snapshot to ~1–2% for near-monotonic Frappe startup.
- PEP 683 (3.14.4): None/True/False, small ints −5..256, empty tuple → immortal (refcount
  0xC0000000). Interned strings + int 257 → mutable refcount (4), NOT immortal.
- THP = always[madvise] but AnonHugePages = 0 in warm worker → did not distort these numbers.

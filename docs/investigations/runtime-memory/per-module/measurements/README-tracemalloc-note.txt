NOTE on tracemalloc.txt — read before using any RSS number from that file.

tracemalloc stores a record per live allocation. With ~337k tracked blocks (importlib alone),
that bookkeeping inflates the *process* RSS ~2.5x. Concretely, in tracemalloc.txt:

    vmrss_kib_after_import = 135,240 KiB   <- INFLATED (real ~60,400 from incremental.csv/isolation.csv)
    vmrss_kib_after_warm   = 294,600 KiB   <- INFLATED (real ~117,000 from incremental.csv/census.txt)

Therefore the rows:
    native_gap_bytes_after_import (104,887,757)
    native_gap_bytes_after_warm   (232,893,502)
are ARTIFACTS — they are (RSS-under-tracemalloc minus traced heap), dominated by tracemalloc's own
overhead. They are NOT a native/C-extension memory estimate and must never be cited as such.
(The current script labels these rows ARTIFACT_IGNORE_*; this raw file predates that rename.)

WHAT IS VALID from tracemalloc.txt:
  - traced_python_bytes_after_import = 33,598,003 B  (~32.0 MiB)  -- real Python-object heap
  - traced_python_bytes_after_warm   = 68,776,898 B  (~65.6 MiB)  -- real Python-object heap
  - the entire per-package table (Python-heap bytes attributed to the allocating module)
  - the top-files table

HONEST native footprint instead: take the clean RSS (incremental.csv / isolation.csv, measured
WITHOUT tracemalloc) and subtract the traced Python heap. Warm worker ~117 MB - ~9.4 MB interpreter
- ~65.6 MB Python heap = ~40 MB native/C-ext/fragmentation (an upper bound; see 00-PER-MODULE-FINDINGS.md).

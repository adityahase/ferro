# cli/ — the `ferro` command

A single-file Python 3 (stdlib-only) CLI modelled on Frappe's `bench`. It drives the
compiled Rust runtime binaries (`ferro` / `ferrod` / `ferro-native`), creates
bench-compatible **forge** workspaces, and runs sites against the SQLite seed.

```
cli/
├── ferro              # the CLI (run as ./cli/ferro or symlink onto PATH)
├── ferro.json         # install manifest: app registry (name -> git/branch) + version
├── VERSION
├── scripts/
│   ├── setup.sh       # install rust, CPython (--enable-shared), jemalloc, node/yarn
│   └── bootstrap.sh   # one-shot: setup -> build -> new-site -> install apps -> verify
└── examples/
    └── api-examples.sh
```

`ferro` resolves its install root (`FERRO_HOME`) as the parent of `cli/` — i.e. the repo
root, where the Rust runtime (`Cargo.toml`, `src/`) and `framework/` live. Override with
`$FERRO_HOME`.

Quick start:

```bash
./cli/ferro setup           # one-time host dependencies
./cli/ferro build           # compile the runtime (ferrod by default)
./cli/ferro new-site dev.localhost
./cli/ferro serve dev.localhost
```

Full command reference and the forge layout: [`../docs/cli.md`](../docs/cli.md).

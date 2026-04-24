# ic-debug — time-travel canister debugger for ICP

A debugger that records every cross-canister call, state diff, cycle burn,
timer trigger, and certified-data update, then lets you replay execution
locally step by step.

See [docs/GUIDE.md](docs/GUIDE.md) for a full walk-through.

## Layout

```
.
├── Cargo.toml            # Rust workspace root
├── rust-toolchain.toml
├── crates/
│   ├── ic-debug-core/          # shared trace event types
│   ├── ic-debug-trace/         # canister-side runtime (trace_state!, trace_event!)
│   ├── ic-debug-trace-macros/  # #[trace_method] proc macro
│   └── ic-debug-cli/           # `ic-debug` CLI: record | replay | diff | serve
├── agent-js/             # ic-debug-agent-js npm package (newTrace / drain)
│   └── src/index.ts
├── ui/                   # React + Vite web UI (timeline + state diff)
├── examples/
│   ├── icp.yaml          # icp-cli deploy config for the demo canisters
│   ├── collect.mjs       # one-shot canister drain utility
│   ├── canisters/
│   │   ├── frontend_api/ # ingress surface — submit_payment (buggy rollback)
│   │   ├── escrow/       # locks funds for a payment
│   │   └── notifications/# delivers receipts (armable failure for demo)
│   └── scripts/
│       ├── 01-hello-trace.mjs   # minimum: one canister, one call
│       ├── 02-multi-canister.mjs# cross-canister trace propagation
│       ├── 03-flag-an-error.mjs # automatic error flagging in the UI
│       └── 04-silent-error.mjs  # what a silent failure looks like
├── docs/GUIDE.md         # user guide
└── schema/               # shared Event schema (JSON Schema + Candid)
```

## Prerequisites

- `icp` CLI — https://github.com/dfinity/icp-cli (`icp --version` should work)
- Rust (`rustup`) — toolchain and wasm target are pinned in `rust-toolchain.toml` and installed automatically
- Node.js ≥ 20

## Quickstart

```bash
# build everything
cargo build --release -p ic-debug-cli
npm --prefix agent-js install && npm --prefix agent-js run build
npm --prefix ui install        && npm --prefix ui run build

# bring up a local replica + deploy the demo canisters
icp network start -d
cd examples && icp deploy --mode reinstall && cd ..

# two terminals: recorder + serve (read-only UI host)
./target/release/ic-debug record --store traces/ic-debug.sqlite --port 9191
./target/release/ic-debug serve  --store traces/ic-debug.sqlite --port 9192 --ui-dir ui/dist

# run the example scripts
node examples/scripts/01-hello-trace.mjs
node examples/scripts/02-multi-canister.mjs
node examples/scripts/03-flag-an-error.mjs
node examples/scripts/04-silent-error.mjs

# open http://127.0.0.1:9192
```

See **[docs/GUIDE.md](docs/GUIDE.md)** for full CLI/UI walk-through,
event model, and instructions for instrumenting your own canisters.

## License

Apache-2.0.

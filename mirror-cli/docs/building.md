# Develop from source

Use the full Mirror checkout, not a Python source archive. Install
[uv](https://docs.astral.sh/uv/getting-started/installation/), Rust, a C compiler,
`protoc`, and ripgrep (`rg`). Run these commands from the checkout root.

## Run

```bash
./mirror-cli/mirror-dev
```

**Check:** the TUI opens. The script builds the Rust worker and runs the Python
CLI from your checkout. It does not replace your installed `mirror`.

## Make a change

Edit `mirror-cli/src/mirror/` for the CLI or `mirror-agent/src/` for the worker.
Exit and rerun `mirror-dev` to rebuild and try the change.

For UI-only work, skip the worker build and use a synthetic conversation:

```bash
./mirror-cli/mirror-dev --showcase
```

**Check:** sending a message plays the demo without model calls or real tools.

## Run checks

```bash
bash mirror-cli/ci.sh tests/cli/main_test.py
```

**Check:** type checking and the selected tests pass. Replace the test path with
your changed tests; omit it to run the full Python suite.

The full local gate also covers the native workspace, Python binding and
hermetic OAuth compatibility fixtures. Use Rust 1.95.0 and Jujutsu 0.43.0 on
`PATH`, with `protoc` and its well-known include path configured for the build:

```bash
python3 -m unittest discover -s diagnostics/claude_oauth/tests -v
cargo +1.95.0 build --locked -p mirror-sandboxd --bin mirror-sandboxd
SANDBOXD_BIN="$PWD/target/debug/mirror-sandboxd" \
  cargo +1.95.0 test --locked --workspace
cargo +1.95.0 check --locked -p mirror-agent-python
RUSTUP_TOOLCHAIN=1.95.0 ./mirror-cli/mirror-dev --help
bash ci.sh
```

Adjust the guest path if `CARGO_TARGET_DIR` is set. The full workspace includes
Linux-specific sandbox fixtures; run that gate on Linux with the real built
guest binary. Python `ci.sh` builds an installed source wheel, checks generated
protobuf imports/types and tests a synthetic native packaging hook. Its dummy
worker cannot establish native-wheel behavior.

## Build a wheel

Only needed to distribute or install your changes:

```bash
bash mirror-cli/build.sh ./dist
```

**Check:** a platform wheel appears in `dist/` after its install checks pass.

Run on each supported release platform. The build uses a fresh Cargo target,
compiles the real agent with pinned ripgrep, cross-builds the Linux sandbox guest
with pinned cargo-zigbuild/Zig, and installs the resulting wheel into a temporary
environment. Install the matching Linux Rust target first (`aarch64-unknown-linux-gnu`
on Apple Silicon/ARM64, `x86_64-unknown-linux-gnu` on x86-64). Set
`RUSTUP_TOOLCHAIN=1.95.0` when the default toolchain differs.

The installed checks retain the OpenAI PTY and bundled read/edit/command tools
tests. Claude tests use isolated HOME directories, real native bytes and worker
paths with spaces: private JSONL, status/logout/missing-login usage, seeded secret
canaries, hidden rejected-callback input, cancellation, terminal restoration,
native process/listener cleanup and preserved OMP notices. They make no provider
requests. `MIRROR_NATIVE_OUTPUT=/absolute/output/native` retains the exact worker,
guest, ripgrep and notices embedded in or used by the wheel.

For acceptance, build from an exact clean commit/archive and record the platform,
command, wheel SHA-256 and matching embedded/staged binary hashes. A supplied
revision environment variable alone is not artifact provenance. Keep the OMP
MIT notice pinned to `df4f996e66a200ab7e0496114d43bdf85f32194d` in source and
native distributions. The [acceptance runbook](../../docs/claude-integration/ACCEPTANCE.md)
separates synthetic gates from explicitly coordinated live model and refresh
checks; the production refresh test is ignored by default and requires an
existing host-local Mirror grant. Never put login callbacks or credential bytes
in build/test logs.

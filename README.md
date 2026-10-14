# Mirror

Mirror is a coding agent for your terminal. It can read and edit files, run
commands, and work on parallel tasks in separate repository worktrees.

## Run from source

Install [uv](https://docs.astral.sh/uv/getting-started/installation/), Rust/Cargo,
a C compiler, `protoc`, and ripgrep (`rg`). uv can download Python 3.12 or later.

```bash
git clone https://github.com/reflection-oss/mirror.git
cd mirror
./mirror-cli/mirror-dev
```

Sign in when prompted, then give Mirror a task. See the
[CLI guide](mirror-cli/README.md#providers-and-models) for other providers and
custom model endpoints.

- `mirror-dev` builds the Rust agent and runs the CLI from your checkout.
- The first run downloads dependencies and compiles the agent; allow a few minutes.
- Rerun after editing. It does not replace an installed `mirror` or check for updates.
- Keep the full checkout: the agent, CLI, and protocol definitions build together.

To work in another project, launch the same script from that project's directory:

```bash
cd /path/to/your/project
/path/to/mirror/mirror-cli/mirror-dev
```

To try the interface without model credentials or a Rust build:

```bash
./mirror-cli/mirror-dev --showcase
```

Send a message to play a synthetic conversation. No model calls or real tools run.

The PyPI package named `mirror` is unrelated; do not install it.
By default, tools run with your account's permissions. Manual approval is not a
sandbox; see [local data and safety](mirror-cli/README.md#local-data-and-safety).

## Development

- [CLI usage and configuration](mirror-cli/README.md)
- [Editing, testing, and building from source](mirror-cli/docs/building.md)
- [Rust agent](mirror-agent/README.md) · [Python embedding](mirror-agent-py/README.md)

```bash
bash ci.sh
cargo test --locked -p mirror-agent -p mirror-protocol
cargo check --locked -p mirror-agent-python
```

The Rust checkpoint tests also require Jujutsu (`jj`) on `PATH`.

## License

[Apache-2.0](LICENSE). Preserve the component notices and third-party licenses
when redistributing source or binaries.

# Mirror

Mirror is a coding agent for your terminal. It can read and edit files, run
commands, and work on parallel tasks in separate repository worktrees.

More [documentation](https://developers.reflection.ai/mirror-overview) is available in Reflection's website.

## Install

Install with
[uv](https://docs.astral.sh/uv/getting-started/installation/):

```bash
uv tool install reflection-mirror
cd /path/to/your/project
mirror
```

The package is named `reflection-mirror`; the command is `mirror`. Release wheels
include the native agent and ripgrep, so no Rust build is required. See the
[supported platforms](mirror-cli/README.md#requirements).

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

By default, tools run with your account's permissions. Manual approval is not a
sandbox; see [local data and safety](mirror-cli/README.md#local-data-and-safety).

The Rust checkpoint tests also require Jujutsu (`jj`) on `PATH`.

## License

[Apache-2.0](LICENSE). Preserve the component notices and third-party licenses
when redistributing source or binaries.

<p align="center"><code>npm i -g @openai/codex</code><br />or <code>brew install --cask codex</code></p>

<p align="center"><strong>Codex CLI</strong> is a coding agent from OpenAI that runs locally on your computer.
</br>
</br>If you want Codex in your code editor (VS Code, Cursor, Windsurf), <a href="https://developers.openai.com/codex/ide">install in your IDE</a>
</br>If you are looking for the <em>cloud-based agent</em> from OpenAI, <strong>Codex Web</strong>, go to <a href="https://chatgpt.com/codex">chatgpt.com/codex</a></p>

<p align="center">
  <img src="./.github/codex-cli-splash.png" alt="Codex CLI splash" width="80%" />
  </p>

---

## Fork Additions

This repository (`thammachet/codex-with-manager`) tracks [openai/codex](https://github.com/openai/codex) but layers several experimental multi-agent capabilities on top:

- **Manager/worker orchestration** – The planning manager runs by default. Disable it with `codex --no-manager`, `codex exec --no-manager`, or `[manager].enabled = false` when you want to talk directly to the worker. The manager uses the new `delegate_worker` tool to spin up full Codex workers that can run commands and edit files while it focuses on breaking work into objectives and summarizing results for you. Worker runs emit `Worker ID: worker-#` so you can resume or close them explicitly, and you can now set `blocking=false` plus `action:"await"`/`"status"` to run many workers in parallel (see `docs/getting-started.md#managerworker-orchestration` and `docs/config.md#manager`).
- **CEO oversight layer** – A higher-level CEO now sits above the manager by default; switch it off with `codex --no-ceo` or `[ceo].enabled = false` when you want manager-only sessions. The CEO never runs tools; it delegates via `delegate_manager`, pushes multiple managers in parallel, and holds them accountable for validation before responding to you. Use `--ceo-model`, `--ceo-reasoning`, and the existing manager flags to assign models and reasoning effort per layer.
- **Layer-specific model + reasoning controls** – Both the manager and the workers can be assigned their own models and reasoning effort via CLI flags (`--manager-model`, `--worker-model`, `--manager-reasoning`, `--worker-reasoning`) or TOML entries. Use `--no-manager`/`--no-ceo` to fall back to the upstream single-agent workflow whenever you need to.
- **UI controls and status signals** – The `/manager` slash command (and the in-TUI manager popup) lets you toggle the layer, switch models, and tune reasoning without leaving the terminal. While the manager waits on a delegate, the status footer now surfaces live updates such as `Running cargo test · worker-3` so you can confirm progress without scrolling.
- **Local helper script** – Run the Rust workspace build directly via `./c-codex <args>` instead of remembering the `cargo run --bin codex` incantation whenever you need to test fork-specific changes.

## Quickstart

### Installing and running Codex CLI

Install globally with your preferred package manager. If you use npm:

```shell
npm install -g @openai/codex
```

Alternatively, if you use Homebrew:

```shell
brew install --cask codex
```

Then simply run `codex` to get started:

```shell
codex
```

If you're running into upgrade issues with Homebrew, see the [FAQ entry on brew upgrade codex](./docs/faq.md#brew-upgrade-codex-isnt-upgrading-me).

<details>
<summary>You can also go to the <a href="https://github.com/openai/codex/releases/latest">latest GitHub Release</a> and download the appropriate binary for your platform.</summary>

Each GitHub Release contains many executables, but in practice, you likely want one of these:

- macOS
  - Apple Silicon/arm64: `codex-aarch64-apple-darwin.tar.gz`
  - x86_64 (older Mac hardware): `codex-x86_64-apple-darwin.tar.gz`
- Linux
  - x86_64: `codex-x86_64-unknown-linux-musl.tar.gz`
  - arm64: `codex-aarch64-unknown-linux-musl.tar.gz`

Each archive contains a single entry with the platform baked into the name (e.g., `codex-x86_64-unknown-linux-musl`), so you likely want to rename it to `codex` after extracting it.

</details>

### Build and install from source

Builds work from canonical or symlinked paths. If you work out of a symlink, set `CARGO_TARGET_DIR` to a stable, realpath-backed directory so Cargo’s cache doesn’t bounce between paths.

```bash
# Clone and enter the Rust workspace
git clone https://github.com/openai/codex.git
cd codex/codex-rs

# Ensure the Rust toolchain is installed
rustup show >/dev/null 2>&1 || curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
rustup component add rustfmt clippy

# Build everything
cargo build --workspace --locked

# Building from a symlink? Pin the target dir once:
#   export CARGO_TARGET_DIR="$(realpath ../codex-target)"  # any stable real path works
#   cargo build --workspace --locked

# Run the CLI directly
cargo run --bin codex -- --help

# Install the CLI binary to ~/.cargo/bin
cargo install --path cli --locked

# If you moved the repo or switched symlink locations, reset the cache:
cargo clean && cargo build --workspace --locked
```

More detail lives in [docs/install.md](./docs/install.md).

### Using Codex with your ChatGPT plan

<p align="center">
  <img src="./.github/codex-cli-login.png" alt="Codex CLI login" width="80%" />
  </p>

Run `codex` and select **Sign in with ChatGPT**. We recommend signing into your ChatGPT account to use Codex as part of your Plus, Pro, Team, Edu, or Enterprise plan. [Learn more about what's included in your ChatGPT plan](https://help.openai.com/en/articles/11369540-codex-in-chatgpt).

You can also use Codex with an API key, but this requires [additional setup](./docs/authentication.md#usage-based-billing-alternative-use-an-openai-api-key). If you previously used an API key for usage-based billing, see the [migration steps](./docs/authentication.md#migrating-from-usage-based-billing-api-key). If you're having trouble with login, please comment on [this issue](https://github.com/openai/codex/issues/1243).

### Model Context Protocol (MCP)

Codex can access MCP servers. To configure them, refer to the [config docs](./docs/config.md#mcp_servers).

### Configuration

Codex CLI supports a rich set of configuration options, with preferences stored in `~/.codex/config.toml`. For full configuration options, see [Configuration](./docs/config.md).

### Execpolicy Quickstart

Codex can enforce your own rules-based execution policy before it runs shell commands.

1. Create a policy directory: `mkdir -p ~/.codex/policy`.
2. Create one or more `.codexpolicy` files in that folder. Codex automatically loads every `.codexpolicy` file in there on startup.
3. Write `prefix_rule` entries to describe the commands you want to allow, prompt, or block:

```starlark
prefix_rule(
    pattern = ["git", ["push", "fetch"]],
    decision = "prompt",  # allow | prompt | forbidden
    match = [["git", "push", "origin", "main"]],  # examples that must match
    not_match = [["git", "status"]],              # examples that must not match
)
```

- `pattern` is a list of shell tokens, evaluated from left to right; wrap tokens in a nested list to express alternatives (e.g., match both `push` and `fetch`).
- `decision` sets the severity; Codex picks the strictest decision when multiple rules match (forbidden > prompt > allow).
- `match` and `not_match` act as (optional) unit tests. Codex validates them when it loads your policy, so you get feedback if an example has unexpected behavior.

In this example rule, if Codex wants to run commands with the prefix `git push` or `git fetch`, it will first ask for user approval.

Use [`execpolicy2` CLI](./codex-rs/execpolicy2/README.md) to preview decisions for policy files:

```shell
cargo run -p codex-execpolicy2 -- check --policy ~/.codex/policy/default.codexpolicy git push origin main
```

Pass multiple `--policy` flags to test how several files combine. See the [`codex-rs/execpolicy2` README](./codex-rs/execpolicy2/README.md) for a more detailed walkthrough of the available syntax.

---

### Docs & FAQ

- [**Getting started**](./docs/getting-started.md)
  - [CLI usage](./docs/getting-started.md#cli-usage)
  - [Slash Commands](./docs/slash_commands.md)
  - [Running with a prompt as input](./docs/getting-started.md#running-with-a-prompt-as-input)
  - [Example prompts](./docs/getting-started.md#example-prompts)
  - [Custom prompts](./docs/prompts.md)
  - [Memory with AGENTS.md](./docs/getting-started.md#memory-with-agentsmd)
- [**Configuration**](./docs/config.md)
  - [Example config](./docs/example-config.md)
- [**Sandbox & approvals**](./docs/sandbox.md)
- [**Authentication**](./docs/authentication.md)
  - [Auth methods](./docs/authentication.md#forcing-a-specific-auth-method-advanced)
  - [Login on a "Headless" machine](./docs/authentication.md#connecting-on-a-headless-machine)
- **Automating Codex**
  - [GitHub Action](https://github.com/openai/codex-action)
  - [TypeScript SDK](./sdk/typescript/README.md)
  - [Non-interactive mode (`codex exec`)](./docs/exec.md)
- [**Advanced**](./docs/advanced.md)
  - [Tracing / verbose logging](./docs/advanced.md#tracing--verbose-logging)
  - [Model Context Protocol (MCP)](./docs/advanced.md#model-context-protocol-mcp)
- [**Zero data retention (ZDR)**](./docs/zdr.md)
- [**Contributing**](./docs/contributing.md)
- [**Install & build**](./docs/install.md)
  - [System Requirements](./docs/install.md#system-requirements)
  - [DotSlash](./docs/install.md#dotslash)
  - [Build from source](./docs/install.md#build-from-source)
- [**FAQ**](./docs/faq.md)
- [**Open source fund**](./docs/open-source-fund.md)

---

## License

This repository is licensed under the [Apache-2.0 License](LICENSE).

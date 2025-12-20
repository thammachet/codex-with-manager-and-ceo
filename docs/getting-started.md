## Getting started

Looking for something specific? Jump ahead:

- [Tips & shortcuts](#tips--shortcuts) – hotkeys, resume flow, prompts
- [Non-interactive runs](./exec.md) – automate with `codex exec`
- Ready for deeper customization? Head to [`advanced.md`](./advanced.md)

### CLI usage

| Command            | Purpose                            | Example                         |
| ------------------ | ---------------------------------- | ------------------------------- |
| `codex`            | Interactive TUI                    | `codex`                         |
| `codex "..."`      | Initial prompt for interactive TUI | `codex "fix lint errors"`       |
| `codex exec "..."` | Non-interactive "automation mode"  | `codex exec "explain utils.ts"` |

Key flags: `--model/-m`, `--ask-for-approval/-a`.

### Resuming interactive sessions

- Run `codex resume` to display the session picker UI
- Resume most recent: `codex resume --last`
- Resume by id: `codex resume <SESSION_ID>` (You can get session ids from /status or `~/.codex/sessions/`)
- The picker shows the session's recorded Git branch when available.
- To show the session's original working directory (CWD), run `codex resume --all` (this also disables cwd filtering and adds a `CWD` column).

Examples:

```shell
# Open a picker of recent sessions
codex resume

# Resume the most recent session
codex resume --last

# Resume a specific session by id
codex resume 7f9f9a2e-1b3c-4c7a-9b0e-123456789abc
```

### Running with a prompt as input

You can also run Codex CLI with a prompt as input:

```shell
codex "explain this codebase to me"
```

### Example prompts

Below are a few bite-size examples you can copy-paste. Replace the text in quotes with your own task.

| ✨  | What you type                                                                   | What happens                                                               |
| --- | ------------------------------------------------------------------------------- | -------------------------------------------------------------------------- |
| 1   | `codex "Refactor the Dashboard component to React Hooks"`                       | Codex rewrites the class component, runs `npm test`, and shows the diff.   |
| 2   | `codex "Generate SQL migrations for adding a users table"`                      | Infers your ORM, creates migration files, and runs them in a sandboxed DB. |
| 3   | `codex "Write unit tests for utils/date.ts"`                                    | Generates tests, executes them, and iterates until they pass.              |
| 4   | `codex "Bulk-rename *.jpeg -> *.jpg with git mv"`                               | Safely renames files and updates imports/usages.                           |
| 5   | `codex "Explain what this regex does: ^(?=.*[A-Z]).{8,}$"`                      | Outputs a step-by-step human explanation.                                  |
| 6   | `codex "Carefully review this repo, and propose 3 high impact well-scoped PRs"` | Suggests impactful PRs in the current codebase.                            |
| 7   | `codex "Look for vulnerabilities and create a security review report"`          | Finds and explains security bugs.                                          |

Looking to reuse your own instructions? Create slash commands with [custom prompts](./prompts.md).

### Memory with AGENTS.md

You can give Codex extra instructions and guidance using `AGENTS.md` files. Codex looks for them in the following places, and merges them top-down:

1. `~/.codex/AGENTS.md` - personal global guidance
2. Every directory from the repository root down to your current working directory (inclusive). In each directory, Codex first looks for `AGENTS.override.md` and uses it if present; otherwise it falls back to `AGENTS.md`. Use the override form when you want to replace inherited instructions for that directory.

For more information on how to use AGENTS.md, see the [official AGENTS.md documentation](https://agents.md/).

### Tips & shortcuts

#### Use `@` for file search

Typing `@` triggers a fuzzy-filename search over the workspace root. Use up/down to select among the results and Tab or Enter to replace the `@` with the selected path. You can use Esc to cancel the search.

#### Esc–Esc to edit a previous message

When the chat composer is empty, press Esc to prime “backtrack” mode. Press Esc again to open a transcript preview highlighting the last user message; press Esc repeatedly to step to older user messages. Press Enter to confirm and Codex will fork the conversation from that point, trim the visible transcript accordingly, and pre‑fill the composer with the selected user message so you can edit and resubmit it.

In the transcript preview, the footer shows an `Esc edit prev` hint while editing is active.

#### `--cd`/`-C` flag

Sometimes it is not convenient to `cd` to the directory you want Codex to use as the "working root" before running Codex. Fortunately, `codex` supports a `--cd` option so you can specify whatever folder you want. You can confirm that Codex is honoring `--cd` by double-checking the **workdir** it reports in the TUI at the start of a new session.

#### `--add-dir` flag

Need to work across multiple projects in one run? Pass `--add-dir` one or more times to expose extra directories as writable roots for the current session while keeping the main working directory unchanged. For example:

```shell
codex --cd apps/frontend --add-dir ../backend --add-dir ../shared
```

Codex can then inspect and edit files in each listed directory without leaving the primary workspace.

#### Manager/worker orchestration

Need a planning layer that can break big requests into smaller sub-tasks? Codex now routes prompts through the manager by default. Use `--no-manager` whenever you want to talk directly to the worker again, and `--manager-model <slug>` / `--worker-model <slug>` to pick the models for each layer. `--manager-reasoning <none|minimal|low|medium|high>` / `--worker-reasoning <…>` pin their reasoning effort. Inside the UI, open the `/manager` command to toggle the manager and adjust its models and reasoning levels without leaving the TUI.

Managers automatically see a condensed list of MCP servers + tools configured for the session, so they can nudge workers to call the right tool without running `/mcp` themselves.

Want each layer to have a distinct personality or role? Pass a `persona` string whenever you call `delegate_worker` (or `delegate_manager`). The text is appended to the delegated agent’s developer instructions before it starts, so you can create specialized workers ("act as the release manager—focus on changelogs and user-facing risk"), or configure a CEO → manager relationship ("be a cleanliness-obsessed architect") without overloading every objective/context payload. Once a worker starts, the persona sticks with it until you close that worker.

Need some workers to research while others only code? Include `"web_search": true` (or `false`) in a `delegate_worker` call to force a worker to include (or exclude) the `web_search` tool regardless of the session’s default feature flag.

Need even more oversight? The CEO layer is enabled by default too; disable it with `--no-ceo` (or `[ceo].enabled = false` in `config.toml`) when you want to skip it. The CEO cannot run tools; it challenges managers, delegates via the new `delegate_manager` tool, and keeps them accountable for PRE_IMPLEMENTATION_PLAN / VALIDATION / PROGRESS_REPORT deliverables before replying to you. Configure it with `--ceo-model <slug>` and `--ceo-reasoning <…>` (or their TOML equivalents). CEOs still rely on the existing manager/worker settings for the layers beneath them, so you can mix-and-match model choices for every stage.

Each delegated agent returns an ID (e.g., `Worker ID: worker-7` or `Manager ID: worker-9`). Include that ID plus `"action": "message"` the next time you call `delegate_worker`/`delegate_manager` to resume the same agent (for example, `{"worker_id":"worker-3","action":"message","objective":"Answer this follow-up"}`). When you're done, send `{ "worker_id": "worker-3", "action": "close" }` (or the manager equivalent) so its resources are released.

Need to fan out multiple subtasks in parallel? Pass `"blocking": false` when starting or messaging a worker to kick off a turn asynchronously. The manager can immediately issue more `delegate_worker` calls (even to brand-new workers) while those turns run concurrently. When you're ready to collect the result, send `{ "worker_id": "worker-3", "action": "await" }`. Use `{ "worker_id": "worker-3", "action": "status" }` for a quick progress snapshot without waiting for completion. Always `await` a worker's pending turn before sending its next objective.

While the manager is waiting on a worker, the status indicator in the bottom pane now shows live updates such as "Running cargo test" or "Calling tool my_server.my_tool" so you can confirm the worker is still progressing without flooding the transcript. When a CEO is coordinating multiple managers, this view expands into a full tree so you can watch each manager’s workers in real time and see the final outcome for every agent even after it finishes.

#### Shell completions

Generate shell completion scripts via:

```shell
codex completion bash
codex completion zsh
codex completion fish
```

#### Image input

Paste images directly into the composer (Ctrl+V / Cmd+V) to attach them to your prompt. You can also attach files via the CLI using `-i/--image` (comma‑separated):

```bash
codex -i screenshot.png "Explain this error"
codex --image img1.png,img2.jpg "Summarize these diagrams"
```

#### Environment variables and executables

Make sure your environment is already set up before launching Codex so it does not spend tokens probing what to activate. For example, source your Python virtualenv (or other language runtimes), start any required daemons, and export the env vars you expect to use ahead of time.

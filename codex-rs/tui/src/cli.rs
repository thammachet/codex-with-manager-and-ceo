use clap::Parser;
use clap::ValueHint;
use codex_common::ApprovalModeCliArg;
use codex_common::CliConfigOverrides;
use codex_common::ReasoningEffortCliArg;
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(version)]
pub struct Cli {
    /// Optional user prompt to start the session.
    #[arg(value_name = "PROMPT", value_hint = clap::ValueHint::Other)]
    pub prompt: Option<String>,

    /// Optional image(s) to attach to the initial prompt.
    #[arg(long = "image", short = 'i', value_name = "FILE", value_delimiter = ',', num_args = 1..)]
    pub images: Vec<PathBuf>,

    // Internal controls set by the top-level `codex resume` subcommand.
    // These are not exposed as user flags on the base `codex` command.
    #[clap(skip)]
    pub resume_picker: bool,

    #[clap(skip)]
    pub resume_last: bool,

    /// Internal: resume a specific recorded session by id (UUID). Set by the
    /// top-level `codex resume <SESSION_ID>` wrapper; not exposed as a public flag.
    #[clap(skip)]
    pub resume_session_id: Option<String>,

    /// Model the agent should use.
    #[arg(long, short = 'm')]
    pub model: Option<String>,

    /// Convenience flag to select the local open source model provider.
    /// Equivalent to -c model_provider=oss; verifies a local Ollama server is
    /// running.
    #[arg(long = "oss", default_value_t = false)]
    pub oss: bool,

    /// Configuration profile from config.toml to specify default options.
    #[arg(long = "profile", short = 'p')]
    pub config_profile: Option<String>,

    /// Select the sandbox policy to use when executing model-generated shell
    /// commands.
    #[arg(long = "sandbox", short = 's')]
    pub sandbox_mode: Option<codex_common::SandboxModeCliArg>,

    /// Configure when the model requires human approval before executing a command.
    #[arg(long = "ask-for-approval", short = 'a')]
    pub approval_policy: Option<ApprovalModeCliArg>,

    /// Convenience alias for low-friction sandboxed automatic execution (-a on-request, --sandbox workspace-write).
    #[arg(long = "full-auto", default_value_t = false)]
    pub full_auto: bool,

    /// Skip all confirmation prompts and execute commands without sandboxing.
    /// EXTREMELY DANGEROUS. Intended solely for running in environments that are externally sandboxed.
    #[arg(
        long = "dangerously-bypass-approvals-and-sandbox",
        alias = "yolo",
        default_value_t = false,
        conflicts_with_all = ["approval_policy", "full_auto"]
    )]
    pub dangerously_bypass_approvals_and_sandbox: bool,

    /// Tell the agent to use the specified directory as its working root.
    #[clap(long = "cd", short = 'C', value_name = "DIR")]
    pub cwd: Option<PathBuf>,

    /// Enable web search (off by default). When enabled, the native Responses `web_search` tool is available to the model (no per‑call approval).
    #[arg(long = "search", default_value_t = false)]
    pub web_search: bool,

    /// Additional directories that should be writable alongside the primary workspace.
    #[arg(long = "add-dir", value_name = "DIR", value_hint = ValueHint::DirPath)]
    pub add_dir: Vec<PathBuf>,

    /// Route prompts through a planning manager agent that orchestrates workers.
    #[arg(
        long = "manager",
        conflicts_with = "no_manager",
        default_value_t = false
    )]
    pub manager: bool,

    /// Disable the manager layer even if enabled in configuration.
    #[arg(long = "no-manager", default_value_t = false)]
    pub no_manager: bool,

    /// Route prompts through a CEO agent that delegates to managers.
    #[arg(long = "ceo", conflicts_with = "no_ceo", default_value_t = false)]
    pub ceo: bool,

    /// Disable the CEO layer even if enabled in configuration.
    #[arg(long = "no-ceo", default_value_t = false)]
    pub no_ceo: bool,

    /// Override the model used for the manager layer.
    #[arg(long = "manager-model")]
    pub manager_model: Option<String>,

    /// Override the model used for worker agents spawned by the manager.
    #[arg(long = "worker-model")]
    pub worker_model: Option<String>,

    /// Override the model used for the CEO layer.
    #[arg(long = "ceo-model")]
    pub ceo_model: Option<String>,

    /// Override the reasoning effort used for the manager (none, minimal, low, medium, high).
    #[arg(long = "manager-reasoning", value_enum)]
    pub manager_reasoning: Option<ReasoningEffortCliArg>,

    /// Override the reasoning effort used for workers (none, minimal, low, medium, high).
    #[arg(long = "worker-reasoning", value_enum)]
    pub worker_reasoning: Option<ReasoningEffortCliArg>,

    /// Override the reasoning effort used for the CEO (none, minimal, low, medium, high).
    #[arg(long = "ceo-reasoning", value_enum)]
    pub ceo_reasoning: Option<ReasoningEffortCliArg>,

    #[clap(skip)]
    pub config_overrides: CliConfigOverrides,
}

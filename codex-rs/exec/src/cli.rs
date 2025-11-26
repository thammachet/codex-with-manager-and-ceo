use clap::Parser;
use clap::ValueEnum;
use codex_common::CliConfigOverrides;
use codex_common::ReasoningEffortCliArg;
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(version)]
pub struct Cli {
    /// Action to perform. If omitted, runs a new non-interactive session.
    #[command(subcommand)]
    pub command: Option<Command>,

    /// Optional image(s) to attach to the initial prompt.
    #[arg(long = "image", short = 'i', value_name = "FILE", value_delimiter = ',', num_args = 1..)]
    pub images: Vec<PathBuf>,

    /// Model the agent should use.
    #[arg(long, short = 'm')]
    pub model: Option<String>,

    /// Use open-source provider.
    #[arg(long = "oss", default_value_t = false)]
    pub oss: bool,

    /// Specify which local provider to use (lmstudio or ollama).
    /// If not specified with --oss, will use config default or show selection.
    #[arg(long = "local-provider")]
    pub oss_provider: Option<String>,

    /// Select the sandbox policy to use when executing model-generated shell
    /// commands.
    #[arg(long = "sandbox", short = 's', value_enum)]
    pub sandbox_mode: Option<codex_common::SandboxModeCliArg>,

    /// Configuration profile from config.toml to specify default options.
    #[arg(long = "profile", short = 'p')]
    pub config_profile: Option<String>,

    /// Convenience alias for low-friction sandboxed automatic execution (-a on-request, --sandbox workspace-write).
    #[arg(long = "full-auto", default_value_t = false)]
    pub full_auto: bool,

    /// Skip all confirmation prompts and execute commands without sandboxing.
    /// EXTREMELY DANGEROUS. Intended solely for running in environments that are externally sandboxed.
    #[arg(
        long = "dangerously-bypass-approvals-and-sandbox",
        alias = "yolo",
        default_value_t = false,
        conflicts_with = "full_auto"
    )]
    pub dangerously_bypass_approvals_and_sandbox: bool,

    /// Tell the agent to use the specified directory as its working root.
    #[clap(long = "cd", short = 'C', value_name = "DIR")]
    pub cwd: Option<PathBuf>,

    /// Allow running Codex outside a Git repository.
    #[arg(long = "skip-git-repo-check", default_value_t = false)]
    pub skip_git_repo_check: bool,

    /// Additional directories that should be writable alongside the primary workspace.
    #[arg(long = "add-dir", value_name = "DIR", value_hint = clap::ValueHint::DirPath)]
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

    /// Path to a JSON Schema file describing the model's final response shape.
    #[arg(long = "output-schema", value_name = "FILE")]
    pub output_schema: Option<PathBuf>,

    #[clap(skip)]
    pub config_overrides: CliConfigOverrides,

    /// Specifies color settings for use in the output.
    #[arg(long = "color", value_enum, default_value_t = Color::Auto)]
    pub color: Color,

    /// Print events to stdout as JSONL.
    #[arg(long = "json", alias = "experimental-json", default_value_t = false)]
    pub json: bool,

    /// Specifies file where the last message from the agent should be written.
    #[arg(long = "output-last-message", short = 'o', value_name = "FILE")]
    pub last_message_file: Option<PathBuf>,

    /// Initial instructions for the agent. If not provided as an argument (or
    /// if `-` is used), instructions are read from stdin.
    #[arg(value_name = "PROMPT", value_hint = clap::ValueHint::Other)]
    pub prompt: Option<String>,
}

#[derive(Debug, clap::Subcommand)]
pub enum Command {
    /// Resume a previous session by id or pick the most recent with --last.
    Resume(ResumeArgs),
}

#[derive(Parser, Debug)]
pub struct ResumeArgs {
    /// Conversation/session id (UUID). When provided, resumes this session.
    /// If omitted, use --last to pick the most recent recorded session.
    #[arg(value_name = "SESSION_ID")]
    pub session_id: Option<String>,

    /// Resume the most recent recorded session (newest) without specifying an id.
    #[arg(long = "last", default_value_t = false)]
    pub last: bool,

    /// Prompt to send after resuming the session. If `-` is used, read from stdin.
    #[arg(value_name = "PROMPT", value_hint = clap::ValueHint::Other)]
    pub prompt: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "kebab-case")]
pub enum Color {
    Always,
    Never,
    #[default]
    Auto,
}

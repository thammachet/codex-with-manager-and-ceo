use clap::ValueEnum;
use codex_protocol::config_types::ReasoningEffort;

/// Clap-compatible reasoning effort selector used by CLI surfaces.
#[derive(Clone, Copy, Debug, ValueEnum, PartialEq, Eq)]
#[value(rename_all = "lowercase")]
pub enum ReasoningEffortCliArg {
    None,
    Minimal,
    Low,
    Medium,
    High,
}

impl From<ReasoningEffortCliArg> for ReasoningEffort {
    fn from(value: ReasoningEffortCliArg) -> Self {
        match value {
            ReasoningEffortCliArg::None => ReasoningEffort::None,
            ReasoningEffortCliArg::Minimal => ReasoningEffort::Minimal,
            ReasoningEffortCliArg::Low => ReasoningEffort::Low,
            ReasoningEffortCliArg::Medium => ReasoningEffort::Medium,
            ReasoningEffortCliArg::High => ReasoningEffort::High,
        }
    }
}

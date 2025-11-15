use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RespondToModelMessage {
    pub content: String,
    pub history_content: Option<String>,
}

impl RespondToModelMessage {
    pub fn new(content: String) -> Self {
        Self {
            content,
            history_content: None,
        }
    }

    pub fn with_history(content: String, history_content: Option<String>) -> Self {
        Self {
            content,
            history_content,
        }
    }
}

impl std::fmt::Display for RespondToModelMessage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.content.fmt(f)
    }
}

impl From<String> for RespondToModelMessage {
    fn from(content: String) -> Self {
        Self::new(content)
    }
}

impl From<&str> for RespondToModelMessage {
    fn from(content: &str) -> Self {
        Self::new(content.to_string())
    }
}

#[derive(Debug, Error, PartialEq)]
pub enum FunctionCallError {
    #[error("{0}")]
    RespondToModel(RespondToModelMessage),
    #[error("{0}")]
    #[allow(dead_code)] // TODO(jif) fix in a follow-up PR
    Denied(String),
    #[error("LocalShellCall without call_id or id")]
    MissingLocalShellCallId,
    #[error("Fatal error: {0}")]
    Fatal(String),
}

#![allow(clippy::expect_used)]
use codex_core::auth::CODEX_API_KEY_ENV_VAR;
use std::fs;
use std::path::Path;
use tempfile::TempDir;
use wiremock::MockServer;

pub struct TestCodexExecBuilder {
    home: TempDir,
    cwd: TempDir,
}

impl TestCodexExecBuilder {
    pub fn cmd(&self) -> assert_cmd::Command {
        let mut cmd = assert_cmd::Command::cargo_bin("codex-exec")
            .expect("should find binary for codex-exec");
        cmd.current_dir(self.cwd.path())
            .env("CODEX_HOME", self.home.path())
            .env(CODEX_API_KEY_ENV_VAR, "dummy");
        cmd
    }
    pub fn cmd_with_server(&self, server: &MockServer) -> assert_cmd::Command {
        let mut cmd = self.cmd();
        let base = format!("{}/v1", server.uri());
        cmd.env("OPENAI_BASE_URL", base);
        cmd
    }

    pub fn cwd_path(&self) -> &Path {
        self.cwd.path()
    }
    pub fn home_path(&self) -> &Path {
        self.home.path()
    }
}

pub fn test_codex_exec() -> TestCodexExecBuilder {
    let home = TempDir::new().expect("create temp home");
    let cwd = TempDir::new().expect("create temp cwd");
    let config = r#"
model = "gpt-4o"
model_provider = "openai"
[features]
apply_patch_freeform = true

[manager]
enabled = false

[ceo]
enabled = false
"#;
    fs::write(home.path().join("config.toml"), config).expect("write config.toml");
    TestCodexExecBuilder { home, cwd }
}

//! Frozen public protocol metadata shared by auth and transport.
//!
//! Source: oh-my-pi df4f996e66a200ab7e0496114d43bdf85f32194d,
//! packages/catalog/src/compat/rules/auth/anthropic.kdl and
//! packages/ai/src/providers/{anthropic,claude-code-fingerprint}.ts.
//! Values match diagnostics/claude_oauth/compatibility.py's reviewed live profile.
//! No runtime version discovery, endpoint override, or credential source belongs here.

pub const PROFILE_ID: &str = "mirror-phase0-omp-df4f996-v1";
pub const OMP_REVISION: &str = "df4f996e66a200ab7e0496114d43bdf85f32194d";
pub const CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
pub const CLI_VERSION: &str = "2.1.280";
pub const SDK_VERSION: &str = "0.112.1";
pub const RUNTIME_VERSION: &str = "v26.3.0";
pub const ANTHROPIC_VERSION: &str = "2023-06-01";
pub const OAUTH_BETA: &str = "oauth-2025-04-20";
pub const SCOPES: &[&str] = &[
    "org:create_api_key",
    "user:profile",
    "user:inference",
    "user:sessions:claude_code",
    "user:mcp_servers",
    "user:file_upload",
];
pub const MESSAGE_BETAS: &[&str] = &[
    "claude-code-20250219",
    OAUTH_BETA,
    "interleaved-thinking-2025-05-14",
    "thinking-token-count-2026-05-13",
    "context-management-2025-06-27",
    "prompt-caching-scope-2026-01-05",
    "mid-conversation-system-2026-04-07",
    "effort-2025-11-24",
    "fallback-credit-2026-06-01",
    "fine-grained-tool-streaming-2025-05-14",
];
pub const IDENTITY_TEXT: &str = "You are Claude Code, Anthropic's official CLI for Claude.";
pub const AUTHORIZATION_URL: &str = "https://claude.ai/oauth/authorize";
pub const API_ORIGIN: &str = "https://api.anthropic.com";
pub const TOKEN_URL: &str = "https://api.anthropic.com/v1/oauth/token";
pub const BOOTSTRAP_URL: &str = "https://api.anthropic.com/api/claude_cli/bootstrap";
pub const MESSAGES_URL: &str = "https://api.anthropic.com/v1/messages";
pub const MODELS_URL: &str = "https://api.anthropic.com/v1/models";
pub const USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";
pub const CALLBACK_HOST: &str = "localhost";
pub const CALLBACK_PORT: u16 = 54545;
pub const CALLBACK_PATH: &str = "/callback";

/// The only external model/account operations an authorization lease can serve.
/// Login, token exchange and bootstrap remain private authority operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaudeOperation {
    Messages,
    Models,
    Usage,
}

impl ClaudeOperation {
    pub const fn endpoint(self) -> &'static str {
        match self {
            Self::Messages => MESSAGES_URL,
            Self::Models => MODELS_URL,
            Self::Usage => USAGE_URL,
        }
    }
}

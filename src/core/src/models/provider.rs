//! The [`Provider`] discriminator and its display name.

use std::fmt;

/// Supported AI coding assistant providers.
///
/// Routes the per-provider usage and analysis roll-ups and labels their display
/// rows. The assistant a session was parsed *from* is
/// [`ExtensionType`](crate::models::ExtensionType), which has no `Unknown`;
/// [`Provider::Unknown`] here is the fallback for a row that belongs to no
/// known provider.
///
/// # Examples
///
/// ```
/// use vct_core::models::Provider;
///
/// assert_eq!(Provider::ClaudeCode.display_name(), "Claude");
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Provider {
    /// Anthropic Claude Code.
    ClaudeCode,
    /// OpenAI Codex CLI.
    Codex,
    /// GitHub Copilot CLI.
    Copilot,
    /// Google Gemini CLI.
    Gemini,
    /// OpenCode.
    OpenCode,
    /// Cursor CLI / IDE.
    Cursor,
    /// Hermes.
    Hermes,
    /// xAI Grok CLI.
    Grok,
    /// DeepSeek Harness (`dsh`).
    DeepSeek,
    /// No known provider.
    Unknown,
}

impl Provider {
    /// Returns the human-readable display name of the provider.
    ///
    /// This is the same string produced by the [`std::fmt::Display`] impl.
    pub const fn display_name(&self) -> &'static str {
        match self {
            Self::ClaudeCode => "Claude",
            Self::Codex => "Codex",
            Self::Copilot => "Copilot",
            Self::Gemini => "Gemini",
            Self::OpenCode => "OpenCode",
            Self::Cursor => "Cursor",
            Self::Hermes => "Hermes",
            Self::Grok => "Grok",
            Self::DeepSeek => "DeepSeek",
            Self::Unknown => "Unknown",
        }
    }
}

impl fmt::Display for Provider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.display_name())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_provider_display() {
        assert_eq!(Provider::ClaudeCode.display_name(), "Claude");
        assert_eq!(Provider::Codex.display_name(), "Codex");
        assert_eq!(Provider::Copilot.display_name(), "Copilot");
        assert_eq!(Provider::Gemini.display_name(), "Gemini");
        assert_eq!(Provider::OpenCode.display_name(), "OpenCode");
        assert_eq!(Provider::Cursor.display_name(), "Cursor");
        assert_eq!(Provider::Hermes.display_name(), "Hermes");
        assert_eq!(Provider::Grok.display_name(), "Grok");
        assert_eq!(Provider::DeepSeek.display_name(), "DeepSeek");
        assert_eq!(Provider::Unknown.display_name(), "Unknown");
    }
}

use std::fmt;

/// Supported AI coding assistant providers.
///
/// Used both to tag a parsed session with its source assistant and to route
/// per-provider usage aggregation. [`Provider::Unknown`] is the fallback when a
/// model name matches none of the known prefixes.
///
/// # Examples
///
/// ```
/// use vct_core::models::Provider;
///
/// assert_eq!(Provider::from_model_name("claude-sonnet-4"), Provider::ClaudeCode);
/// assert_eq!(Provider::ClaudeCode.display_name(), "Claude");
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Provider {
    /// Anthropic Claude Code.
    ClaudeCode,
    /// OpenAI Codex CLI (also matches raw `gpt-*` / `o1` / `o3` model names).
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
    /// Model name matched no known provider prefix.
    Unknown,
}

impl Provider {
    /// Detects the AI provider from a model name using byte-level prefix matching.
    ///
    /// Recognizes the `claude`, `copilot`, `gemini`, and `grok` prefixes, and
    /// treats `gpt*` / `o1*` / `o3*` model names as [`Provider::Codex`].
    /// Matching is case-sensitive (lowercase) and operates directly on the
    /// UTF-8 bytes, so it stays `const` and allocation-free. Returns
    /// [`Provider::Unknown`] when no prefix matches.
    ///
    /// # Examples
    ///
    /// ```
    /// use vct_core::models::Provider;
    ///
    /// assert_eq!(Provider::from_model_name("gpt-4-turbo"), Provider::Codex);
    /// assert_eq!(Provider::from_model_name("o3-mini"), Provider::Codex);
    /// assert_eq!(Provider::from_model_name("gemini-2.0-flash"), Provider::Gemini);
    /// assert_eq!(Provider::from_model_name("mystery-model"), Provider::Unknown);
    /// ```
    pub const fn from_model_name(model: &str) -> Self {
        // Use byte comparison for better performance
        let bytes = model.as_bytes();

        if bytes.len() >= 6 {
            // Check for "claude" prefix
            if bytes[0] == b'c'
                && bytes[1] == b'l'
                && bytes[2] == b'a'
                && bytes[3] == b'u'
                && bytes[4] == b'd'
                && bytes[5] == b'e'
            {
                return Self::ClaudeCode;
            }
        }

        // Check for "copilot" prefix
        if bytes.len() >= 7
            && bytes[0] == b'c'
            && bytes[1] == b'o'
            && bytes[2] == b'p'
            && bytes[3] == b'i'
            && bytes[4] == b'l'
            && bytes[5] == b'o'
            && bytes[6] == b't'
        {
            return Self::Copilot;
        }

        if bytes.len() >= 6
            && bytes[0] == b'g'
            && bytes[1] == b'e'
            && bytes[2] == b'm'
            && bytes[3] == b'i'
            && bytes[4] == b'n'
            && bytes[5] == b'i'
        {
            return Self::Gemini;
        }

        if bytes.len() >= 4
            && bytes[0] == b'g'
            && bytes[1] == b'r'
            && bytes[2] == b'o'
            && bytes[3] == b'k'
        {
            return Self::Grok;
        }

        // Check for OpenAI/Codex models
        if bytes.len() >= 3 && bytes[0] == b'g' && bytes[1] == b'p' && bytes[2] == b't' {
            return Self::Codex;
        }

        if bytes.len() >= 2 && bytes[0] == b'o' && (bytes[1] == b'1' || bytes[1] == b'3') {
            return Self::Codex;
        }

        Self::Unknown
    }

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
    fn test_provider_detection() {
        assert_eq!(
            Provider::from_model_name("claude-sonnet-4"),
            Provider::ClaudeCode
        );
        assert_eq!(
            Provider::from_model_name("claude-3-opus"),
            Provider::ClaudeCode
        );
        assert_eq!(Provider::from_model_name("gpt-4-turbo"), Provider::Codex);
        assert_eq!(Provider::from_model_name("gpt-3.5"), Provider::Codex);
        assert_eq!(Provider::from_model_name("o1-preview"), Provider::Codex);
        assert_eq!(Provider::from_model_name("o3-mini"), Provider::Codex);
        assert_eq!(Provider::from_model_name("copilot"), Provider::Copilot);
        assert_eq!(
            Provider::from_model_name("copilot-gpt-4"),
            Provider::Copilot
        );
        assert_eq!(Provider::from_model_name("gemini-pro"), Provider::Gemini);
        assert_eq!(
            Provider::from_model_name("gemini-2.0-flash"),
            Provider::Gemini
        );
        assert_eq!(Provider::from_model_name("grok-4.5"), Provider::Grok);
        assert_eq!(
            Provider::from_model_name("unknown-model"),
            Provider::Unknown
        );
    }

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
        assert_eq!(Provider::Unknown.display_name(), "Unknown");
    }
}

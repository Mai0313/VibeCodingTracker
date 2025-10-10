use std::fmt;

/// Supported AI coding assistant providers
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Provider {
    ClaudeCode,
    Codex,
    Gemini,
    Unknown,
}

impl Provider {
    /// Detects the AI provider from a model name using byte-level pattern matching
    ///
    /// This const function enables compile-time optimization and uses efficient byte
    /// comparison to identify Claude, Gemini, or Codex models.
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

        // Check for OpenAI/Codex models
        if bytes.len() >= 3 && bytes[0] == b'g' && bytes[1] == b'p' && bytes[2] == b't' {
            return Self::Codex;
        }

        if bytes.len() >= 2 && bytes[0] == b'o' && (bytes[1] == b'1' || bytes[1] == b'3') {
            return Self::Codex;
        }

        Self::Unknown
    }

    /// Returns the human-readable name of the provider
    pub const fn display_name(&self) -> &'static str {
        match self {
            Self::ClaudeCode => "Claude Code",
            Self::Codex => "OpenAI Codex",
            Self::Gemini => "Gemini",
            Self::Unknown => "Unknown",
        }
    }

    /// Returns the emoji icon representing the provider
    pub const fn icon(&self) -> &'static str {
        match self {
            Self::ClaudeCode => "🤖",
            Self::Codex => "🧠",
            Self::Gemini => "✨",
            Self::Unknown => "❓",
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
        assert_eq!(Provider::from_model_name("gemini-pro"), Provider::Gemini);
        assert_eq!(
            Provider::from_model_name("gemini-2.0-flash"),
            Provider::Gemini
        );
        assert_eq!(
            Provider::from_model_name("unknown-model"),
            Provider::Unknown
        );
    }

    #[test]
    fn test_provider_display() {
        assert_eq!(Provider::ClaudeCode.display_name(), "Claude Code");
        assert_eq!(Provider::Codex.display_name(), "OpenAI Codex");
        assert_eq!(Provider::Gemini.display_name(), "Gemini");
        assert_eq!(Provider::Unknown.display_name(), "Unknown");
    }

    #[test]
    fn test_provider_icon() {
        assert_eq!(Provider::ClaudeCode.icon(), "🤖");
        assert_eq!(Provider::Codex.icon(), "🧠");
        assert_eq!(Provider::Gemini.icon(), "✨");
        assert_eq!(Provider::Unknown.icon(), "❓");
    }
}

use std::fmt;

/// Supported terminal emulators.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Terminal {
    Ghostty,
    ITerm2,
    TerminalApp,
    Unknown(String),
}

impl fmt::Display for Terminal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Terminal::Ghostty => write!(f, "Ghostty"),
            Terminal::ITerm2 => write!(f, "iTerm2"),
            Terminal::TerminalApp => write!(f, "Terminal.app"),
            Terminal::Unknown(name) => write!(f, "{name}"),
        }
    }
}

/// How popup rendering achieves flicker-free output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderStrategy {
    /// DECSET 2026 synchronized output — terminal buffers until end marker.
    Synchronized,
    /// Build entire frame into buffer, emit in single write() call.
    PreRenderBuffer,
}

impl fmt::Display for RenderStrategy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RenderStrategy::Synchronized => write!(f, "Synchronized (DECSET 2026)"),
            RenderStrategy::PreRenderBuffer => write!(f, "PreRenderBuffer (single write)"),
        }
    }
}

/// How prompt boundaries are detected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptDetection {
    /// Native OSC 133 semantic prompt markers (terminal forwards them).
    Osc133,
    /// Custom OSC 7771 emitted by shell integration (terminal-agnostic).
    ShellIntegration,
}

impl fmt::Display for PromptDetection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PromptDetection::Osc133 => write!(f, "OSC 133 (native)"),
            PromptDetection::ShellIntegration => write!(f, "OSC 7771 (shell integration)"),
        }
    }
}

/// Terminal capabilities detected at startup.
#[derive(Debug, Clone)]
pub struct TerminalProfile {
    pub terminal: Terminal,
    pub render_strategy: RenderStrategy,
    pub prompt_detection: PromptDetection,
    pub name: String,
}

impl TerminalProfile {
    /// Detect the current terminal and build a capability profile.
    ///
    /// Reads `TERM_PROGRAM` (and tmux-specific env vars) to identify
    /// the terminal and set appropriate strategies.
    pub fn detect() -> Self {
        let term_program = std::env::var("TERM_PROGRAM").unwrap_or_default();
        let in_tmux = std::env::var("TMUX").is_ok();

        // Direct terminal detection (not inside tmux)
        if !in_tmux {
            return Self::from_term_program(&term_program);
        }

        // Inside tmux: terminal info comes from env vars that leak through
        if std::env::var("GHOSTTY_RESOURCES_DIR").is_ok() {
            return Self::new(Terminal::Ghostty, "Ghostty (via tmux)");
        }
        if std::env::var("ITERM_SESSION_ID").is_ok() {
            return Self::new(Terminal::ITerm2, "iTerm2 (via tmux)");
        }

        // Fallback: try TERM_PROGRAM (some terminals set it even in tmux)
        Self::from_term_program(&term_program)
    }

    fn from_term_program(term_program: &str) -> Self {
        match term_program {
            "ghostty" => Self::new(Terminal::Ghostty, "Ghostty"),
            "iTerm.app" => Self::new(Terminal::ITerm2, "iTerm2"),
            "Apple_Terminal" => Self::new(Terminal::TerminalApp, "Terminal.app"),
            "" => Self::new(Terminal::Unknown("unknown".into()), "unknown"),
            other => Self::new(Terminal::Unknown(other.into()), other),
        }
    }

    fn new(terminal: Terminal, name: &str) -> Self {
        let (render_strategy, prompt_detection) = match &terminal {
            Terminal::Ghostty => (RenderStrategy::Synchronized, PromptDetection::Osc133),
            Terminal::ITerm2 => (
                RenderStrategy::PreRenderBuffer,
                PromptDetection::ShellIntegration,
            ),
            Terminal::TerminalApp => (
                RenderStrategy::PreRenderBuffer,
                PromptDetection::ShellIntegration,
            ),
            Terminal::Unknown(_) => (
                RenderStrategy::PreRenderBuffer,
                PromptDetection::ShellIntegration,
            ),
        };

        tracing::info!(
            terminal = %terminal,
            render = %render_strategy,
            prompt = %prompt_detection,
            "detected terminal profile"
        );

        Self {
            terminal,
            render_strategy,
            prompt_detection,
            name: name.to_string(),
        }
    }
}

/// Check whether a `TERM_PROGRAM` value is in the supported allowlist.
pub fn is_supported(term_program: &str) -> bool {
    matches!(term_program, "ghostty" | "iTerm.app" | "Apple_Terminal")
}

/// List of supported terminal `TERM_PROGRAM` values (for diagnostics).
pub const SUPPORTED_TERMINALS: &[&str] = &["ghostty", "iTerm.app", "Apple_Terminal"];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_supported() {
        assert!(is_supported("ghostty"));
        assert!(is_supported("iTerm.app"));
        assert!(is_supported("Apple_Terminal"));
        assert!(!is_supported("alacritty"));
        assert!(!is_supported(""));
        assert!(!is_supported("WezTerm"));
    }

    #[test]
    fn test_ghostty_profile() {
        let profile = TerminalProfile::from_term_program("ghostty");
        assert_eq!(profile.terminal, Terminal::Ghostty);
        assert_eq!(profile.render_strategy, RenderStrategy::Synchronized);
        assert_eq!(profile.prompt_detection, PromptDetection::Osc133);
    }

    #[test]
    fn test_iterm2_profile() {
        let profile = TerminalProfile::from_term_program("iTerm.app");
        assert_eq!(profile.terminal, Terminal::ITerm2);
        assert_eq!(profile.render_strategy, RenderStrategy::PreRenderBuffer);
        assert_eq!(profile.prompt_detection, PromptDetection::ShellIntegration);
    }

    #[test]
    fn test_terminal_app_profile() {
        let profile = TerminalProfile::from_term_program("Apple_Terminal");
        assert_eq!(profile.terminal, Terminal::TerminalApp);
        assert_eq!(profile.render_strategy, RenderStrategy::PreRenderBuffer);
        assert_eq!(profile.prompt_detection, PromptDetection::ShellIntegration);
    }

    #[test]
    fn test_unknown_terminal_profile() {
        let profile = TerminalProfile::from_term_program("alacritty");
        assert!(matches!(profile.terminal, Terminal::Unknown(_)));
        assert_eq!(profile.render_strategy, RenderStrategy::PreRenderBuffer);
        assert_eq!(profile.prompt_detection, PromptDetection::ShellIntegration);
    }

    #[test]
    fn test_empty_term_program() {
        let profile = TerminalProfile::from_term_program("");
        assert!(matches!(profile.terminal, Terminal::Unknown(_)));
        assert_eq!(profile.name, "unknown");
    }

    #[test]
    fn test_terminal_display() {
        assert_eq!(Terminal::Ghostty.to_string(), "Ghostty");
        assert_eq!(Terminal::ITerm2.to_string(), "iTerm2");
        assert_eq!(Terminal::TerminalApp.to_string(), "Terminal.app");
        assert_eq!(Terminal::Unknown("foo".into()).to_string(), "foo");
    }

    #[test]
    fn test_render_strategy_display() {
        assert!(RenderStrategy::Synchronized
            .to_string()
            .contains("DECSET 2026"));
        assert!(RenderStrategy::PreRenderBuffer
            .to_string()
            .contains("single write"));
    }

    #[test]
    fn test_supported_terminals_list() {
        assert_eq!(SUPPORTED_TERMINALS.len(), 3);
        for t in SUPPORTED_TERMINALS {
            assert!(is_supported(t));
        }
    }
}

use std::fmt;

/// Supported terminal emulators.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Terminal {
    Ghostty,
    Kitty,
    WezTerm,
    Alacritty,
    Rio,
    ITerm2,
    TerminalApp,
    Unknown(String),
}

impl Terminal {
    /// Whether this is a known/supported terminal (not `Unknown`).
    pub fn is_known(&self) -> bool {
        !matches!(self, Terminal::Unknown(_))
    }

    /// Display names of all supported terminals (for diagnostics).
    pub fn supported_terminals() -> &'static [&'static str] {
        &[
            "Ghostty",
            "Kitty",
            "WezTerm",
            "Alacritty",
            "Rio",
            "iTerm2",
            "Terminal.app",
        ]
    }
}

impl fmt::Display for Terminal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Terminal::Ghostty => write!(f, "Ghostty"),
            Terminal::Kitty => write!(f, "Kitty"),
            Terminal::WezTerm => write!(f, "WezTerm"),
            Terminal::Alacritty => write!(f, "Alacritty"),
            Terminal::Rio => write!(f, "Rio"),
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
///
/// Fields are private to enforce the invariant that `render_strategy` and
/// `prompt_detection` are always derived from the `Terminal` variant.
/// Use `detect()` or the `for_*()` constructors (tests only).
#[derive(Debug, Clone)]
pub struct TerminalProfile {
    terminal: Terminal,
    render_strategy: RenderStrategy,
    prompt_detection: PromptDetection,
    in_tmux: bool,
}

impl TerminalProfile {
    pub fn terminal(&self) -> &Terminal {
        &self.terminal
    }
    pub fn render_strategy(&self) -> RenderStrategy {
        self.render_strategy
    }
    pub fn prompt_detection(&self) -> PromptDetection {
        self.prompt_detection
    }
    pub fn in_tmux(&self) -> bool {
        self.in_tmux
    }

    /// Human-readable display name, e.g. "Ghostty" or "iTerm2 (via tmux)".
    pub fn display_name(&self) -> String {
        let base = self.terminal.to_string();
        if self.in_tmux {
            format!("{base} (via tmux)")
        } else {
            base
        }
    }

    /// Detect the current terminal and build a capability profile.
    ///
    /// Checks terminal-specific env vars and `TERM_PROGRAM` to identify
    /// the terminal and set appropriate strategies.
    pub fn detect() -> Self {
        let term_program = std::env::var("TERM_PROGRAM").unwrap_or_default();
        let in_tmux = std::env::var("TMUX").is_ok();
        let has_ghostty_res = std::env::var("GHOSTTY_RESOURCES_DIR").is_ok();
        let has_iterm_session = std::env::var("ITERM_SESSION_ID").is_ok();
        let has_kitty_window_id = std::env::var("KITTY_WINDOW_ID").is_ok();
        let has_wezterm_socket = std::env::var("WEZTERM_UNIX_SOCKET").is_ok();
        let has_alacritty_socket = std::env::var("ALACRITTY_SOCKET").is_ok();

        Self::detect_from_env(
            &term_program,
            in_tmux,
            has_ghostty_res,
            has_iterm_session,
            has_kitty_window_id,
            has_wezterm_socket,
            has_alacritty_socket,
        )
    }

    /// Testable detection logic — takes env values as parameters.
    fn detect_from_env(
        term_program: &str,
        in_tmux: bool,
        has_ghostty_res: bool,
        has_iterm_session: bool,
        has_kitty_window_id: bool,
        has_wezterm_socket: bool,
        has_alacritty_socket: bool,
    ) -> Self {
        // Direct terminal detection (not inside tmux)
        if !in_tmux {
            // Kitty doesn't set TERM_PROGRAM — detect via KITTY_WINDOW_ID
            if has_kitty_window_id {
                return Self::new(Terminal::Kitty, false);
            }
            return Self::from_term_program(term_program, false);
        }

        // Inside tmux: terminal info comes from env vars that leak through.
        // Check terminal-specific env vars before falling back to TERM_PROGRAM.
        if has_ghostty_res {
            return Self::new(Terminal::Ghostty, true);
        }
        if has_kitty_window_id {
            return Self::new(Terminal::Kitty, true);
        }
        if has_wezterm_socket {
            return Self::new(Terminal::WezTerm, true);
        }
        if has_alacritty_socket {
            return Self::new(Terminal::Alacritty, true);
        }
        if has_iterm_session {
            return Self::new(Terminal::ITerm2, true);
        }

        // Fallback: try TERM_PROGRAM (some terminals set it even in tmux)
        Self::from_term_program(term_program, true)
    }

    fn from_term_program(term_program: &str, in_tmux: bool) -> Self {
        let terminal = match term_program {
            "ghostty" => Terminal::Ghostty,
            "iTerm.app" => Terminal::ITerm2,
            "Apple_Terminal" => Terminal::TerminalApp,
            "WezTerm" => Terminal::WezTerm,
            "alacritty" => Terminal::Alacritty,
            "rio" => Terminal::Rio,
            "" => Terminal::Unknown("unknown".into()),
            other => Terminal::Unknown(other.into()),
        };
        Self::new(terminal, in_tmux)
    }

    fn new(terminal: Terminal, in_tmux: bool) -> Self {
        let (render_strategy, prompt_detection) = match &terminal {
            // Full native support: synchronized output + OSC 133 prompt markers
            Terminal::Ghostty | Terminal::Kitty | Terminal::WezTerm | Terminal::Rio => {
                (RenderStrategy::Synchronized, PromptDetection::Osc133)
            }
            // Synchronized output but no native OSC 133 (Alacritty issue open since 2022)
            Terminal::Alacritty => (
                RenderStrategy::Synchronized,
                PromptDetection::ShellIntegration,
            ),
            // Legacy: no DECSET 2026, use pre-render buffer + shell integration
            Terminal::ITerm2 | Terminal::TerminalApp | Terminal::Unknown(_) => (
                RenderStrategy::PreRenderBuffer,
                PromptDetection::ShellIntegration,
            ),
        };

        Self {
            terminal,
            render_strategy,
            prompt_detection,
            in_tmux,
        }
    }

    /// Test constructor: Ghostty profile (Synchronized, OSC 133).
    pub fn for_ghostty() -> Self {
        Self::new(Terminal::Ghostty, false)
    }

    /// Test constructor: iTerm2 profile (PreRenderBuffer, ShellIntegration).
    pub fn for_iterm2() -> Self {
        Self::new(Terminal::ITerm2, false)
    }

    /// Test constructor: Terminal.app profile (PreRenderBuffer, ShellIntegration).
    pub fn for_terminal_app() -> Self {
        Self::new(Terminal::TerminalApp, false)
    }

    /// Test constructor: Kitty profile (Synchronized, OSC 133).
    pub fn for_kitty() -> Self {
        Self::new(Terminal::Kitty, false)
    }

    /// Test constructor: WezTerm profile (Synchronized, OSC 133).
    pub fn for_wezterm() -> Self {
        Self::new(Terminal::WezTerm, false)
    }

    /// Test constructor: Alacritty profile (Synchronized, ShellIntegration).
    pub fn for_alacritty() -> Self {
        Self::new(Terminal::Alacritty, false)
    }

    /// Test constructor: Rio profile (Synchronized, OSC 133).
    pub fn for_rio() -> Self {
        Self::new(Terminal::Rio, false)
    }

    /// Test constructor: Unknown terminal profile (PreRenderBuffer, ShellIntegration).
    pub fn for_unknown(name: &str) -> Self {
        Self::new(Terminal::Unknown(name.into()), false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- Terminal methods --

    #[test]
    fn test_terminal_is_known() {
        assert!(Terminal::Ghostty.is_known());
        assert!(Terminal::Kitty.is_known());
        assert!(Terminal::WezTerm.is_known());
        assert!(Terminal::Alacritty.is_known());
        assert!(Terminal::Rio.is_known());
        assert!(Terminal::ITerm2.is_known());
        assert!(Terminal::TerminalApp.is_known());
        assert!(!Terminal::Unknown("foot".into()).is_known());
        assert!(!Terminal::Unknown("unknown".into()).is_known());
    }

    #[test]
    fn test_supported_terminals_count() {
        // 7 supported terminals: Ghostty, Kitty, WezTerm, Alacritty, Rio, iTerm2, Terminal.app
        assert_eq!(Terminal::supported_terminals().len(), 7);
    }

    // -- Profile from TERM_PROGRAM --

    #[test]
    fn test_ghostty_profile() {
        let profile = TerminalProfile::from_term_program("ghostty", false);
        assert_eq!(*profile.terminal(), Terminal::Ghostty);
        assert_eq!(profile.render_strategy(), RenderStrategy::Synchronized);
        assert_eq!(profile.prompt_detection(), PromptDetection::Osc133);
        assert!(!profile.in_tmux());
    }

    #[test]
    fn test_iterm2_profile() {
        let profile = TerminalProfile::from_term_program("iTerm.app", false);
        assert_eq!(*profile.terminal(), Terminal::ITerm2);
        assert_eq!(profile.render_strategy(), RenderStrategy::PreRenderBuffer);
        assert_eq!(
            profile.prompt_detection(),
            PromptDetection::ShellIntegration
        );
    }

    #[test]
    fn test_terminal_app_profile() {
        let profile = TerminalProfile::from_term_program("Apple_Terminal", false);
        assert_eq!(*profile.terminal(), Terminal::TerminalApp);
        assert_eq!(profile.render_strategy(), RenderStrategy::PreRenderBuffer);
        assert_eq!(
            profile.prompt_detection(),
            PromptDetection::ShellIntegration
        );
    }

    #[test]
    fn test_wezterm_profile() {
        let profile = TerminalProfile::from_term_program("WezTerm", false);
        assert_eq!(*profile.terminal(), Terminal::WezTerm);
        assert_eq!(profile.render_strategy(), RenderStrategy::Synchronized);
        assert_eq!(profile.prompt_detection(), PromptDetection::Osc133);
    }

    #[test]
    fn test_alacritty_profile() {
        let profile = TerminalProfile::from_term_program("alacritty", false);
        assert_eq!(*profile.terminal(), Terminal::Alacritty);
        assert_eq!(profile.render_strategy(), RenderStrategy::Synchronized);
        assert_eq!(
            profile.prompt_detection(),
            PromptDetection::ShellIntegration
        );
    }

    #[test]
    fn test_rio_profile() {
        let profile = TerminalProfile::from_term_program("rio", false);
        assert_eq!(*profile.terminal(), Terminal::Rio);
        assert_eq!(profile.render_strategy(), RenderStrategy::Synchronized);
        assert_eq!(profile.prompt_detection(), PromptDetection::Osc133);
    }

    #[test]
    fn test_unknown_terminal_profile() {
        let profile = TerminalProfile::from_term_program("foot", false);
        assert!(matches!(profile.terminal(), Terminal::Unknown(_)));
        assert_eq!(profile.render_strategy(), RenderStrategy::PreRenderBuffer);
        assert_eq!(
            profile.prompt_detection(),
            PromptDetection::ShellIntegration
        );
    }

    #[test]
    fn test_empty_term_program() {
        let profile = TerminalProfile::from_term_program("", false);
        assert!(matches!(profile.terminal(), Terminal::Unknown(_)));
        assert_eq!(profile.display_name(), "unknown");
    }

    // -- Display --

    #[test]
    fn test_terminal_display() {
        assert_eq!(Terminal::Ghostty.to_string(), "Ghostty");
        assert_eq!(Terminal::Kitty.to_string(), "Kitty");
        assert_eq!(Terminal::WezTerm.to_string(), "WezTerm");
        assert_eq!(Terminal::Alacritty.to_string(), "Alacritty");
        assert_eq!(Terminal::Rio.to_string(), "Rio");
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
    fn test_display_name_with_tmux() {
        let profile = TerminalProfile::new(Terminal::Ghostty, true);
        assert_eq!(profile.display_name(), "Ghostty (via tmux)");
        let profile = TerminalProfile::new(Terminal::ITerm2, false);
        assert_eq!(profile.display_name(), "iTerm2");
    }

    // -- detect_from_env (direct + tmux paths) --

    // Helper: call detect_from_env with only the specified flags set.
    fn detect(
        term_program: &str,
        in_tmux: bool,
        ghostty_res: bool,
        iterm_session: bool,
        kitty_wid: bool,
        wezterm_sock: bool,
        alacritty_sock: bool,
    ) -> TerminalProfile {
        TerminalProfile::detect_from_env(
            term_program,
            in_tmux,
            ghostty_res,
            iterm_session,
            kitty_wid,
            wezterm_sock,
            alacritty_sock,
        )
    }

    #[test]
    fn test_detect_ghostty_direct() {
        let p = detect("ghostty", false, false, false, false, false, false);
        assert_eq!(*p.terminal(), Terminal::Ghostty);
        assert!(!p.in_tmux());
    }

    #[test]
    fn test_detect_kitty_direct() {
        // Kitty doesn't set TERM_PROGRAM — detected via KITTY_WINDOW_ID
        let p = detect("", false, false, false, true, false, false);
        assert_eq!(*p.terminal(), Terminal::Kitty);
        assert!(!p.in_tmux());
    }

    #[test]
    fn test_detect_wezterm_direct() {
        let p = detect("WezTerm", false, false, false, false, false, false);
        assert_eq!(*p.terminal(), Terminal::WezTerm);
        assert!(!p.in_tmux());
    }

    #[test]
    fn test_detect_alacritty_direct() {
        let p = detect("alacritty", false, false, false, false, false, false);
        assert_eq!(*p.terminal(), Terminal::Alacritty);
        assert!(!p.in_tmux());
    }

    #[test]
    fn test_detect_rio_direct() {
        let p = detect("rio", false, false, false, false, false, false);
        assert_eq!(*p.terminal(), Terminal::Rio);
        assert!(!p.in_tmux());
    }

    #[test]
    fn test_detect_ghostty_via_tmux() {
        let p = detect("", true, true, false, false, false, false);
        assert_eq!(*p.terminal(), Terminal::Ghostty);
        assert!(p.in_tmux());
        assert_eq!(p.display_name(), "Ghostty (via tmux)");
    }

    #[test]
    fn test_detect_kitty_via_tmux() {
        let p = detect("", true, false, false, true, false, false);
        assert_eq!(*p.terminal(), Terminal::Kitty);
        assert!(p.in_tmux());
        assert_eq!(p.display_name(), "Kitty (via tmux)");
    }

    #[test]
    fn test_detect_wezterm_via_tmux() {
        let p = detect("", true, false, false, false, true, false);
        assert_eq!(*p.terminal(), Terminal::WezTerm);
        assert!(p.in_tmux());
        assert_eq!(p.display_name(), "WezTerm (via tmux)");
    }

    #[test]
    fn test_detect_alacritty_via_tmux() {
        let p = detect("", true, false, false, false, false, true);
        assert_eq!(*p.terminal(), Terminal::Alacritty);
        assert!(p.in_tmux());
        assert_eq!(p.display_name(), "Alacritty (via tmux)");
    }

    #[test]
    fn test_detect_iterm2_via_tmux() {
        let p = detect("", true, false, true, false, false, false);
        assert_eq!(*p.terminal(), Terminal::ITerm2);
        assert!(p.in_tmux());
        assert_eq!(p.display_name(), "iTerm2 (via tmux)");
    }

    #[test]
    fn test_detect_tmux_ghostty_takes_priority_over_iterm() {
        let p = detect("", true, true, true, false, false, false);
        assert_eq!(*p.terminal(), Terminal::Ghostty);
    }

    #[test]
    fn test_detect_tmux_ghostty_takes_priority_over_kitty() {
        let p = detect("", true, true, false, true, false, false);
        assert_eq!(*p.terminal(), Terminal::Ghostty);
    }

    #[test]
    fn test_detect_tmux_kitty_takes_priority_over_wezterm() {
        let p = detect("", true, false, false, true, true, false);
        assert_eq!(*p.terminal(), Terminal::Kitty);
    }

    #[test]
    fn test_detect_tmux_falls_back_to_term_program() {
        let p = detect("Apple_Terminal", true, false, false, false, false, false);
        assert_eq!(*p.terminal(), Terminal::TerminalApp);
        assert!(p.in_tmux());
    }

    #[test]
    fn test_detect_tmux_unknown_terminal() {
        let p = detect("", true, false, false, false, false, false);
        assert!(matches!(p.terminal(), Terminal::Unknown(_)));
        assert!(p.in_tmux());
    }

    // -- Test constructors --

    #[test]
    fn test_for_ghostty() {
        let p = TerminalProfile::for_ghostty();
        assert_eq!(*p.terminal(), Terminal::Ghostty);
        assert_eq!(p.render_strategy(), RenderStrategy::Synchronized);
        assert_eq!(p.prompt_detection(), PromptDetection::Osc133);
    }

    #[test]
    fn test_for_kitty() {
        let p = TerminalProfile::for_kitty();
        assert_eq!(*p.terminal(), Terminal::Kitty);
        assert_eq!(p.render_strategy(), RenderStrategy::Synchronized);
        assert_eq!(p.prompt_detection(), PromptDetection::Osc133);
    }

    #[test]
    fn test_for_wezterm() {
        let p = TerminalProfile::for_wezterm();
        assert_eq!(*p.terminal(), Terminal::WezTerm);
        assert_eq!(p.render_strategy(), RenderStrategy::Synchronized);
        assert_eq!(p.prompt_detection(), PromptDetection::Osc133);
    }

    #[test]
    fn test_for_alacritty() {
        let p = TerminalProfile::for_alacritty();
        assert_eq!(*p.terminal(), Terminal::Alacritty);
        assert_eq!(p.render_strategy(), RenderStrategy::Synchronized);
        assert_eq!(p.prompt_detection(), PromptDetection::ShellIntegration);
    }

    #[test]
    fn test_for_rio() {
        let p = TerminalProfile::for_rio();
        assert_eq!(*p.terminal(), Terminal::Rio);
        assert_eq!(p.render_strategy(), RenderStrategy::Synchronized);
        assert_eq!(p.prompt_detection(), PromptDetection::Osc133);
    }

    #[test]
    fn test_for_iterm2() {
        let p = TerminalProfile::for_iterm2();
        assert_eq!(*p.terminal(), Terminal::ITerm2);
        assert_eq!(p.render_strategy(), RenderStrategy::PreRenderBuffer);
    }

    #[test]
    fn test_for_terminal_app() {
        let p = TerminalProfile::for_terminal_app();
        assert_eq!(*p.terminal(), Terminal::TerminalApp);
        assert_eq!(p.render_strategy(), RenderStrategy::PreRenderBuffer);
    }

    #[test]
    fn test_for_unknown() {
        let p = TerminalProfile::for_unknown("foot");
        assert!(matches!(p.terminal(), Terminal::Unknown(s) if s == "foot"));
        assert_eq!(p.render_strategy(), RenderStrategy::PreRenderBuffer);
    }

    // -- Case sensitivity: TERM_PROGRAM matching is exact --

    #[test]
    fn test_capitalized_ghostty_is_unknown() {
        let profile = TerminalProfile::from_term_program("Ghostty", false);
        assert!(matches!(profile.terminal(), Terminal::Unknown(_)));
    }

    #[test]
    fn test_lowercase_iterm_is_unknown() {
        let profile = TerminalProfile::from_term_program("iterm.app", false);
        assert!(matches!(profile.terminal(), Terminal::Unknown(_)));
    }

    #[test]
    fn test_lowercase_apple_terminal_is_unknown() {
        let profile = TerminalProfile::from_term_program("apple_terminal", false);
        assert!(matches!(profile.terminal(), Terminal::Unknown(_)));
    }

    #[test]
    fn test_lowercase_wezterm_is_unknown() {
        let profile = TerminalProfile::from_term_program("wezterm", false);
        assert!(matches!(profile.terminal(), Terminal::Unknown(_)));
    }

    #[test]
    fn test_capitalized_alacritty_is_unknown() {
        let profile = TerminalProfile::from_term_program("Alacritty", false);
        assert!(matches!(profile.terminal(), Terminal::Unknown(_)));
    }

    // -- PromptDetection display --

    #[test]
    fn test_prompt_detection_display() {
        assert!(PromptDetection::Osc133.to_string().contains("133"));
        assert!(PromptDetection::ShellIntegration
            .to_string()
            .contains("7771"));
    }
}

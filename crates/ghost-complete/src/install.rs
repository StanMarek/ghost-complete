use anyhow::{Context, Result};
use std::fs;
use std::io::Write as _;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use crate::sanitize::{sanitize_for_terminal, sanitize_path};

// `EMBEDDED_SPECS` was moved into `gc-suggest` so the runtime spec loader can
// fall back to it when no on-disk spec dir is found. Install still needs to
// write the same set to `~/.config/ghost-complete/specs`, so we re-export
// from the canonical home; `status.rs` and the install tests reach it
// through `crate::install::EMBEDDED_SPECS` exactly as before.
pub(crate) use gc_suggest::EMBEDDED_SPECS;

const ZSH_INTEGRATION: &str = include_str!("../../../shell/ghost-complete.zsh");
const ZSH_INIT: &str = include_str!("../../../shell/init.zsh");

const DEFAULT_CONFIG_TOML: &str = "\
# Ghost Complete configuration
# Uncomment and edit values to customize. All values shown are defaults.

# [trigger]
# auto_chars = [' ', '/', '-', '.']
# delay_ms = 150
# auto_trigger = true  # Set to false to disable all automatic triggers (manual keybinding only)

# [popup]
# max_visible = 10
# borders = false  # Set to true to enable rounded borders around the popup

# [suggest]
# max_results = 50
# max_history_results = 5
# generator_timeout_ms = 5000  # Per-invocation timeout (ms) for async script/git generators

# [suggest.providers]
# commands = true
# filesystem = true
# specs = true
# git = true

# [keybindings]
# accept = \"tab\"
# accept_and_enter = \"enter\"
# dismiss = \"escape\"
# navigate_up = \"arrow_up\"
# navigate_down = \"arrow_down\"
# trigger = \"ctrl+/\"

[theme]
# preset = \"dark\"
# selected = \"reverse\"
# description = \"dim\"
# match_highlight = \"bold\"
# item_text = \"\"
# scrollbar = \"dim\"
# border = \"dim\"

# [experimental]
# multi_terminal = false  # Set to true to enable unsupported/unknown terminals
";

const INIT_BEGIN: &str = "# >>> ghost-complete initialize >>>";
const INIT_END: &str = "# <<< ghost-complete initialize <<<";
const SHELL_BEGIN: &str = "# >>> ghost-complete shell integration >>>";
const SHELL_END: &str = "# <<< ghost-complete shell integration <<<";
const MANAGED_WARNING: &str =
    "# !! Contents within this block are managed by 'ghost-complete install' !!";

/// Single-quote a path for safe embedding in shell code.
/// Escapes embedded single quotes with the `'\''` idiom.
///
/// Also strips ASCII/C1 control characters (ESC, BEL, NUL, CSI, etc.) from
/// the path text before quoting. The resulting snippet is later printed to
/// the user's terminal by `print_shell_blocks`, so a `$HOME`/config-derived
/// path containing crafted control bytes would otherwise be evaluated by
/// the terminal — single-quoting does not neutralise terminal escapes, only
/// shell metacharacters. Single-quote escaping happens after sanitisation
/// so that a legitimate single quote embedded in the path is still handled
/// correctly by the `'\''` idiom.
fn shell_safe_path(path: &Path) -> String {
    let s = sanitize_for_terminal(&path.display().to_string());
    format!("'{}'", s.replace('\'', r"'\''"))
}

fn init_block(script_path: &Path) -> String {
    let path = shell_safe_path(script_path);
    format!(
        "{INIT_BEGIN}\n\
         {MANAGED_WARNING}\n\
         if [[ -f {path} ]]; then\n  \
         builtin source {path}\n\
         else\n  \
         echo \"ghost-complete: init script missing: \"{path} >&2\n  \
         echo \"ghost-complete: run 'ghost-complete install' to restore it\" >&2\n\
         fi\n\
         {INIT_END}"
    )
}

fn shell_integration_block(script_path: &Path) -> String {
    format!(
        "{SHELL_BEGIN}\n\
         {MANAGED_WARNING}\n\
         source {}\n\
         {SHELL_END}",
        shell_safe_path(script_path)
    )
}

/// Strips a managed block delimited by `begin`..`end` markers from `content`.
/// Returns `(new_content, was_found)`.
fn remove_block(content: &str, begin: &str, end: &str) -> (String, bool) {
    let mut content = content.to_string();
    let mut found = false;

    while let Some(start_idx) = content.find(begin) {
        let Some(end_match) = content[start_idx..].find(end) else {
            break;
        };
        let end_idx = start_idx + end_match + end.len();

        let mut result = String::with_capacity(content.len());
        result.push_str(&content[..start_idx]);
        // Skip trailing newline after end marker if present
        let after = if content[end_idx..].starts_with('\n') {
            &content[end_idx + 1..]
        } else {
            &content[end_idx..]
        };
        result.push_str(after);

        content = result;
        found = true;
    }

    (content, found)
}

fn copy_specs(config_dir: &Path) -> Result<()> {
    let dest = config_dir.join("specs");
    fs::create_dir_all(&dest).with_context(|| format!("failed to create {}", dest.display()))?;

    let mut count = 0;
    for (name, contents) in EMBEDDED_SPECS {
        let dest_file = dest.join(name);
        fs::write(&dest_file, contents)
            .with_context(|| format!("failed to write spec: {}", dest_file.display()))?;
        count += 1;
    }
    println!(
        "  Installed {count} completion specs to {}",
        sanitize_path(&dest)
    );
    Ok(())
}

fn print_shell_blocks(init_path: &Path, script_path: &Path) {
    let init = init_block(init_path);
    let shell = shell_integration_block(script_path);
    let indented_init = init.replace('\n', "\n    ");
    let indented_shell = shell.replace('\n', "\n    ");

    println!(
        "  \x1b[36m\u{2139}\x1b[0m  Add the following \x1b[1mNEAR THE TOP\x1b[0m of your shell config:\n"
    );
    println!("    \x1b[36m{indented_init}\x1b[0m\n");
    println!(
        "  \x1b[36m\u{2139}\x1b[0m  Add the following \x1b[1mNEAR THE BOTTOM\x1b[0m of your shell config:\n"
    );
    println!("    \x1b[36m{indented_shell}\x1b[0m\n");
}

fn post_install_summary(config_dir: &Path, wrote_zshrc: bool) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();

    writeln!(
        out,
        "\x1b[32m\u{2713}\x1b[0m  ghost-complete installed successfully!"
    )
    .unwrap();
    writeln!(out).unwrap();

    writeln!(out, "\x1b[1mNext steps:\x1b[0m").unwrap();
    if wrote_zshrc {
        writeln!(
            out,
            "  1. Restart your shell:    \x1b[1msource ~/.zshrc\x1b[0m"
        )
        .unwrap();
    } else {
        writeln!(
            out,
            "  1. Restart your shell after pasting the blocks above."
        )
        .unwrap();
    }
    writeln!(
        out,
        "  2. Verify the install:    \x1b[1mghost-complete doctor\x1b[0m"
    )
    .unwrap();
    writeln!(
        out,
        "  3. Try it:                \x1b[1mcd /tmp && git \x1b[0m\x1b[2m[space]\x1b[0m"
    )
    .unwrap();
    writeln!(
        out,
        "  4. Manual trigger:        \x1b[1mCtrl+/\x1b[0m  (if the popup doesn't appear)"
    )
    .unwrap();
    writeln!(
        out,
        "  5. Customize:             \x1b[1mghost-complete config edit\x1b[0m"
    )
    .unwrap();
    writeln!(out).unwrap();

    writeln!(out, "\x1b[1mFiles installed:\x1b[0m").unwrap();
    writeln!(
        out,
        "  Config:  {}",
        sanitize_path(&config_dir.join("config.toml"))
    )
    .unwrap();
    writeln!(
        out,
        "  Specs:   {}/  ({} completion specs)",
        sanitize_path(&config_dir.join("specs")),
        EMBEDDED_SPECS.len()
    )
    .unwrap();
    writeln!(out).unwrap();

    writeln!(
        out,
        "Docs: https://github.com/StanMarek/ghost-complete#readme"
    )
    .unwrap();

    out
}

fn install_to(zshrc_path: &Path, config_dir: &Path, dry_run: bool) -> Result<()> {
    // 1. Write zsh shell scripts
    let shell_dir = config_dir.join("shell");
    let init_path = shell_dir.join("init.zsh");
    let script_path = shell_dir.join("ghost-complete.zsh");

    if dry_run {
        println!("  Would write init script to {}", sanitize_path(&init_path));
        println!(
            "  Would write zsh integration to {}",
            sanitize_path(&script_path)
        );
        println!(
            "  Would install {} completion specs to {}",
            EMBEDDED_SPECS.len(),
            sanitize_path(&config_dir.join("specs"))
        );
        let config_path = config_dir.join("config.toml");
        if !config_path.exists() {
            println!(
                "  Would write default config to {}",
                sanitize_path(&config_path)
            );
        } else {
            println!("  Config already exists at {}", sanitize_path(&config_path));
        }
        println!("  Would update {}\n", sanitize_path(zshrc_path));
        println!("  \x1b[36m\u{2139}\x1b[0m  The following would be added to your shell config:\n");
        print_shell_blocks(&init_path, &script_path);
        return Ok(());
    }

    fs::create_dir_all(&shell_dir)
        .with_context(|| format!("failed to create {}", shell_dir.display()))?;

    fs::write(&init_path, ZSH_INIT)
        .with_context(|| format!("failed to write {}", init_path.display()))?;
    println!("  Wrote init script to {}", sanitize_path(&init_path));

    fs::write(&script_path, ZSH_INTEGRATION)
        .with_context(|| format!("failed to write {}", script_path.display()))?;
    println!("  Wrote zsh integration to {}", sanitize_path(&script_path));

    // 1b. Copy completion specs
    copy_specs(config_dir)?;

    // 1c. Write default config.toml if one doesn't exist (never clobber).
    // Uses create_new(true) so the existence check and file creation are
    // a single atomic operation — closes the TOCTOU race where two
    // concurrent `ghost-complete install` runs could clobber a config
    // written between the exists() check and the subsequent write.
    let config_path = config_dir.join("config.toml");
    match fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&config_path)
    {
        Ok(mut file) => {
            file.write_all(DEFAULT_CONFIG_TOML.as_bytes())
                .with_context(|| format!("failed to write {}", config_path.display()))?;
            println!("  Wrote default config to {}", sanitize_path(&config_path));
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            println!("  Config already exists at {}", sanitize_path(&config_path));
        }
        Err(e) => {
            return Err(anyhow::Error::new(e))
                .with_context(|| format!("failed to create {}", config_path.display()));
        }
    }

    // 2. Read existing .zshrc (or empty)
    let existing = if zshrc_path.exists() {
        fs::read_to_string(zshrc_path)
            .with_context(|| format!("failed to read {}", zshrc_path.display()))?
    } else {
        String::new()
    };

    // 3. Backup (only on first install — preserve the original).
    // Uses create_new(true) so the existence check and file creation are
    // a single atomic operation — closes the TOCTOU race where two
    // concurrent `ghost-complete install` runs could clobber an existing
    // backup between the exists() check and the subsequent copy.
    //
    // Mode preservation: the old `fs::copy` call mirrored the source file's
    // permissions on the backup. We replicate that here explicitly — create
    // the file with a restrictive 0o600 so no other process can open a
    // half-written world-readable copy, then chmod to match the source after
    // the write completes. This preserves restrictive modes like 0o600 that
    // security-conscious users may have set on .zshrc containing secrets.
    if zshrc_path.exists() {
        let backup = zshrc_path.with_extension("backup.ghost-complete");
        let src_perms = fs::metadata(zshrc_path)
            .with_context(|| format!("failed to stat {}", zshrc_path.display()))?
            .permissions();
        let zshrc_bytes = fs::read(zshrc_path)
            .with_context(|| format!("failed to read {}", zshrc_path.display()))?;
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&backup)
        {
            Ok(mut file) => {
                file.write_all(&zshrc_bytes)
                    .with_context(|| format!("failed to backup to {}", backup.display()))?;
                // Match fs::copy semantics: mirror the source file's mode
                // on the backup, bypassing umask.
                fs::set_permissions(&backup, src_perms).with_context(|| {
                    format!("failed to set permissions on {}", backup.display())
                })?;
                println!("  Backed up .zshrc to {}", sanitize_path(&backup));
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                println!("  Backup already exists at {}", sanitize_path(&backup));
            }
            Err(e) => {
                return Err(anyhow::Error::new(e))
                    .with_context(|| format!("failed to backup to {}", backup.display()));
            }
        }
    }

    // 4. Strip existing managed blocks (idempotent)
    let (content, _) = remove_block(&existing, INIT_BEGIN, INIT_END);
    let (content, _) = remove_block(&content, SHELL_BEGIN, SHELL_END);

    // 5. Prepend init block, append shell integration block
    let content = content.trim().to_string();
    let mut new_zshrc = String::new();
    new_zshrc.push_str(&init_block(&init_path));
    new_zshrc.push('\n');
    if !content.is_empty() {
        new_zshrc.push_str(&content);
        new_zshrc.push('\n');
    }
    new_zshrc.push_str(&shell_integration_block(&script_path));
    new_zshrc.push('\n');

    // 6. Write .zshrc — graceful fallback if permission denied (e.g. nix-managed)
    match fs::write(zshrc_path, &new_zshrc) {
        Ok(()) => {
            println!("  Updated {}", sanitize_path(zshrc_path));
            print!("\n{}", post_install_summary(config_dir, true));
        }
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            println!(
                "\n  \x1b[33m\u{26a0}  Could not write to {} (permission denied)\x1b[0m\n",
                sanitize_path(zshrc_path)
            );
            print_shell_blocks(&init_path, &script_path);
            println!(
                "  \x1b[32m\u{2713}\x1b[0m  Installation complete (manual shell configuration required)."
            );
            print!("\n{}", post_install_summary(config_dir, false));
        }
        Err(e) => {
            return Err(anyhow::anyhow!(
                "failed to write {}: {}",
                zshrc_path.display(),
                e
            ));
        }
    }
    Ok(())
}

fn uninstall_from(zshrc_path: &Path, config_dir: &Path) -> Result<()> {
    // 1. Strip managed blocks from .zshrc
    if zshrc_path.exists() {
        let content = fs::read_to_string(zshrc_path)
            .with_context(|| format!("failed to read {}", zshrc_path.display()))?;

        let (content, found_init) = remove_block(&content, INIT_BEGIN, INIT_END);
        let (content, found_shell) = remove_block(&content, SHELL_BEGIN, SHELL_END);

        if found_init || found_shell {
            fs::write(zshrc_path, &content)
                .with_context(|| format!("failed to write {}", zshrc_path.display()))?;
            println!(
                "  Removed managed blocks from {}",
                sanitize_path(zshrc_path)
            );
        } else {
            println!(
                "  No ghost-complete blocks found in {}",
                sanitize_path(zshrc_path)
            );
        }
    } else {
        println!(
            "  {} does not exist, nothing to do",
            sanitize_path(zshrc_path)
        );
    }

    // 2. Remove shell integration scripts
    for name in &[
        "init.zsh",
        "ghost-complete.zsh",
        "ghost-complete.bash",
        "ghost-complete.fish",
    ] {
        let script_path = config_dir.join("shell").join(name);
        if script_path.exists() {
            fs::remove_file(&script_path)
                .with_context(|| format!("failed to remove {}", script_path.display()))?;
            println!("  Removed {}", sanitize_path(&script_path));
        }
    }

    // 3. Clean up empty shell/ directory (best-effort)
    let shell_dir = config_dir.join("shell");
    if shell_dir.exists() {
        let _ = fs::remove_dir(&shell_dir); // only succeeds if empty
    }

    // 4. Note about retained files
    let specs_dir = config_dir.join("specs");
    let has_specs =
        specs_dir.exists() && fs::read_dir(&specs_dir).is_ok_and(|mut d| d.next().is_some());
    let has_config = config_dir.join("config.toml").exists();
    if has_specs || has_config {
        eprintln!();
        eprintln!("  \x1b[33mNote:\x1b[0m The following files were retained:");
        if has_specs {
            eprintln!(
                "    - {} ({} specs)",
                sanitize_path(&specs_dir),
                fs::read_dir(&specs_dir).map(|d| d.count()).unwrap_or(0)
            );
        }
        if has_config {
            eprintln!("    - {}", sanitize_path(&config_dir.join("config.toml")));
        }
        eprintln!(
            "  To remove everything: rm -rf {}",
            sanitize_path(config_dir)
        );
    }

    println!("\nghost-complete uninstalled successfully!");
    Ok(())
}

pub fn run_install(dry_run: bool) -> Result<()> {
    // Guard: refuse to install as root — creates root-owned files that break normal user's shell
    // SAFETY: libc::getuid() has no preconditions on POSIX and cannot fail.
    // It performs a single read of the real user ID from the kernel and
    // returns it. No pointer safety, no FFI lifetime concerns, no error path.
    if unsafe { libc::getuid() } == 0 {
        anyhow::bail!(
            "refusing to install as root — this would create root-owned files in your \
             home directory that break shell startup. Run without sudo."
        );
    }

    let home = dirs::home_dir().context("could not determine home directory")?;
    let zshrc = home.join(".zshrc");
    let config_dir = gc_config::config_dir().context("could not determine home directory")?;

    if dry_run {
        println!("Dry run: ghost-complete install\n");
    } else {
        println!("Installing ghost-complete...\n");
    }
    install_to(&zshrc, &config_dir, dry_run)
}

pub fn run_uninstall() -> Result<()> {
    let home = dirs::home_dir().context("could not determine home directory")?;
    let zshrc = home.join(".zshrc");
    let config_dir = gc_config::config_dir().context("could not determine home directory")?;

    println!("Uninstalling ghost-complete...\n");
    uninstall_from(&zshrc, &config_dir)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn test_remove_block_basic() {
        let content = "before\n# >>> ghost-complete initialize >>>\nstuff\n# <<< ghost-complete initialize <<<\nafter\n";
        let (result, found) = remove_block(content, INIT_BEGIN, INIT_END);
        assert!(found);
        assert_eq!(result, "before\nafter\n");
        assert!(!result.contains("ghost-complete initialize"));
    }

    #[test]
    fn test_remove_block_not_found() {
        let content = "just some shell config\nexport FOO=bar\n";
        let (result, found) = remove_block(content, INIT_BEGIN, INIT_END);
        assert!(!found);
        assert_eq!(result, content);
    }

    #[test]
    fn test_init_block_content() {
        let path = Path::new("/some/path/init.zsh");
        let block = init_block(path);
        assert!(block.contains(INIT_BEGIN));
        assert!(block.contains(INIT_END));
        assert!(block.contains(MANAGED_WARNING));
        // Source line pointing to external script (single-quoted)
        assert!(block.contains("builtin source '/some/path/init.zsh'"));
        assert!(block.contains("-f '/some/path/init.zsh'"));
        // Missing-file warning (else branch)
        assert!(block.contains("ghost-complete: init script missing:"));
        assert!(block.contains("ghost-complete install"));
    }

    #[test]
    fn test_init_script_content() {
        // Verify the external init script has all the required detection logic
        let script = ZSH_INIT;
        assert!(script.contains("__ghost_complete_init()"));
        assert!(script.contains("unset -f __ghost_complete_init"));
        assert!(script.contains("exec ghost-complete"));
        assert!(script.contains("command -v ghost-complete"));

        // --- Structural validation: guards must be in the correct branch ---

        // Split on the else branch to get tmux vs non-tmux sections
        let tmux_marker = "if [[ -n \"$TMUX\" ]]; then";
        assert!(script.contains(tmux_marker), "missing tmux branch");
        let tmux_start = script.find(tmux_marker).unwrap();
        let else_marker = script[tmux_start..].find("\n  else\n").unwrap();
        let tmux_branch = &script[tmux_start..tmux_start + else_marker];
        let non_tmux_branch = &script[tmux_start + else_marker..];

        // tmux branch: PPID check (quoted) + GHOST_COMPLETE_PANE check
        assert!(
            tmux_branch.contains("ps -o comm= -p \"$PPID\""),
            "tmux branch must have quoted PPID check"
        );
        assert!(
            tmux_branch.contains("GHOST_COMPLETE_PANE"),
            "tmux branch must have GHOST_COMPLETE_PANE subshell guard"
        );
        assert!(
            tmux_branch.contains("$TMUX_PANE"),
            "tmux branch must compare GHOST_COMPLETE_PANE against TMUX_PANE"
        );
        // tmux branch must NOT use GHOST_COMPLETE_ACTIVE as a guard
        assert!(
            !tmux_branch.contains("[[ -n \"$GHOST_COMPLETE_ACTIVE\" ]] && return"),
            "tmux branch must not use GHOST_COMPLETE_ACTIVE as recursion guard"
        );

        // non-tmux branch: GHOST_COMPLETE_ACTIVE guard
        assert!(
            non_tmux_branch.contains("GHOST_COMPLETE_ACTIVE"),
            "non-tmux branch must use GHOST_COMPLETE_ACTIVE guard"
        );

        // tmux branch: detect outer terminal via env vars
        assert!(tmux_branch.contains("$GHOSTTY_RESOURCES_DIR"));
        assert!(tmux_branch.contains("$KITTY_WINDOW_ID"));
        assert!(tmux_branch.contains("$WEZTERM_UNIX_SOCKET"));
        assert!(tmux_branch.contains("$ALACRITTY_SOCKET"));
        assert!(tmux_branch.contains("$ITERM_SESSION_ID"));
        assert!(tmux_branch.contains("\"$TERM_PROGRAM\" == \"rio\""));
        assert!(tmux_branch.contains("$ZED_TERM"));
        assert!(tmux_branch.contains("$VSCODE_IPC_HOOK_CLI"));

        // Direct terminal detection (non-tmux)
        assert!(non_tmux_branch.contains("case \"$TERM_PROGRAM\""));
        assert!(
            non_tmux_branch.contains("ghostty|WezTerm|rio|iTerm.app|Apple_Terminal|zed|vscode)")
        );
        assert!(non_tmux_branch.contains("$ZED_TERM"));
        assert!(non_tmux_branch.contains("$VSCODE_IPC_HOOK_CLI"));

        // Ancestor-walk-based reset for inherited GHOST_COMPLETE_ACTIVE so the
        // `code .` flow still wires the proxy into VSCode's integrated terminal
        // when the env var propagated from the launching shell — while still
        // honoring the guard for subshells whose $PPID is another shell.
        assert!(
            non_tmux_branch.contains("_gc_ancestor_is_proxy"),
            "non-tmux branch must walk PPID ancestry to distinguish subshell \
             from leaked env var"
        );
        assert!(
            non_tmux_branch.contains("unset GHOST_COMPLETE_ACTIVE"),
            "non-tmux branch must reset inherited GHOST_COMPLETE_ACTIVE when \
             ancestry walk confirms leak"
        );
        // Both branches must coexist: honor the guard when the walk says
        // "descendant" (0) or "uncertain/ps failed" (2), and reset only when
        // the walk confirms the var is leaked (1).
        assert!(
            non_tmux_branch.matches("return").count() >= 2,
            "non-tmux branch must honor the guard on both 'descendant' and \
             'ps uncertain' outcomes"
        );
    }

    #[test]
    fn test_zsh_integration_native_osc133_helper() {
        assert!(
            ZSH_INTEGRATION.contains("_gc_native_osc133()"),
            "zsh integration must define _gc_native_osc133 helper"
        );
        assert!(ZSH_INTEGRATION.contains("ZED_TERM"));
        assert!(ZSH_INTEGRATION.contains("VSCODE_INJECTION"));
        assert!(
            ZSH_INTEGRATION.contains("ghostty")
                || ZSH_INTEGRATION.contains("GHOSTTY_RESOURCES_DIR"),
            "helper must cover Ghostty"
        );
        assert!(
            ZSH_INTEGRATION.contains("_gc_native_osc133 || printf '\\e]7771;A"),
            "_gc_precmd must suppress OSC 7771 when terminal parses OSC 133 natively"
        );
        assert!(
            ZSH_INTEGRATION.contains("_gc_native_osc133 || printf '\\e]7771;C"),
            "_gc_preexec must suppress OSC 7771 when terminal parses OSC 133 natively"
        );
    }

    #[test]
    fn test_zsh_integration_vscode_injection_not_ipc_hook() {
        // Extract just the _gc_native_osc133 helper body to avoid matching
        // the unrelated __ghost_complete_init block (which does check
        // VSCODE_IPC_HOOK_CLI). The semantic split: detection uses the IPC
        // hook (gc-terminal), suppression uses INJECTION (shell integration).
        let start = ZSH_INTEGRATION
            .find("_gc_native_osc133()")
            .expect("helper must be defined");
        let after = &ZSH_INTEGRATION[start..];
        let end = after.find("\n}").expect("helper must have closing brace");
        let helper_body = &after[..end];

        assert!(
            helper_body.contains("VSCODE_INJECTION"),
            "suppression helper must check VSCODE_INJECTION (the shell-integration signal)"
        );
        assert!(
            !helper_body.contains("VSCODE_IPC_HOOK_CLI"),
            "suppression helper must NOT check VSCODE_IPC_HOOK_CLI — that env var is for \
             detection, not suppression. Confusing the two would silently disable OSC 7771 \
             for VSCode users who have the integrated terminal open but haven't enabled \
             VSCode's shell integration."
        );
    }

    #[test]
    fn test_shell_integration_block_content() {
        let path = Path::new("/some/path/ghost-complete.zsh");
        let block = shell_integration_block(path);
        assert!(block.contains(SHELL_BEGIN));
        assert!(block.contains(SHELL_END));
        assert!(block.contains(MANAGED_WARNING));
        assert!(block.contains("source '/some/path/ghost-complete.zsh'"));
    }

    #[test]
    fn test_shell_safe_path_escapes_metacharacters() {
        // Dollar sign — would trigger variable expansion in double quotes
        let path = Path::new("/home/$USER/config/init.zsh");
        assert_eq!(shell_safe_path(path), "'/home/$USER/config/init.zsh'");

        // Backtick — would trigger command substitution in double quotes
        let path = Path::new("/home/user`whoami`/init.zsh");
        assert_eq!(shell_safe_path(path), "'/home/user`whoami`/init.zsh'");

        // Double quote — would break double-quoted embedding
        let path = Path::new("/home/us\"er/init.zsh");
        assert_eq!(shell_safe_path(path), "'/home/us\"er/init.zsh'");

        // Single quote — must be escaped with '\'' idiom
        let path = Path::new("/home/o'brien/init.zsh");
        assert_eq!(shell_safe_path(path), r"'/home/o'\''brien/init.zsh'");

        // Combined metacharacters
        let path = Path::new("/home/$(`evil'cmd\")/init.zsh");
        assert_eq!(
            shell_safe_path(path),
            r#"'/home/$(`evil'\''cmd")/init.zsh'"#
        );

        // Space in path — must be single-quoted to prevent word splitting
        let path = Path::new("/home/my user/config/init.zsh");
        assert_eq!(shell_safe_path(path), "'/home/my user/config/init.zsh'");

        // Tab in path — control character is stripped to prevent terminal
        // escape-sequence smuggling via `print_shell_blocks`, which prints
        // the rendered snippet directly to stdout.
        let path = Path::new("/home/user\t/init.zsh");
        assert_eq!(shell_safe_path(path), "'/home/user/init.zsh'");

        // Newline in path — control character is stripped to prevent both
        // terminal escape-sequence smuggling and shell command injection
        // via `$'\nrm -rf ~'`-style exploits.
        let path = Path::new("/home/user\n/init.zsh");
        assert_eq!(shell_safe_path(path), "'/home/user/init.zsh'");
    }

    #[test]
    fn test_shell_safe_path_strips_control_bytes() {
        // ESC-based CSI sequence: must be stripped before it reaches the
        // user's terminal via `print_shell_blocks`. Single-quote shell
        // escaping does NOT neutralise terminal escapes.
        let path = Path::new("/home/alice/\x1b[31mevil\x1b[0m/init.zsh");
        let quoted = shell_safe_path(path);
        assert!(
            !quoted.contains('\x1b'),
            "ESC byte must be stripped from shell snippet, got: {quoted:?}"
        );
        assert_eq!(quoted, "'/home/alice/[31mevil[0m/init.zsh'");

        // BEL (bell) character: commonly terminates OSC sequences.
        let path = Path::new("/home/\x07bob/init.zsh");
        let quoted = shell_safe_path(path);
        assert!(!quoted.contains('\x07'));
        assert_eq!(quoted, "'/home/bob/init.zsh'");

        // Sanitisation happens before single-quote escaping, so a legitimate
        // apostrophe in the path is still escaped correctly afterwards.
        let path = Path::new("/home/o'brien/\x1b[31mx/init.zsh");
        let quoted = shell_safe_path(path);
        assert!(!quoted.contains('\x1b'));
        assert_eq!(quoted, r"'/home/o'\''brien/[31mx/init.zsh'");
    }

    #[test]
    fn test_print_shell_blocks_sanitizes_paths() {
        // End-to-end: a `$HOME`/config-derived path containing ESC bytes
        // must not appear verbatim in the snippet emitted by the install
        // blocks. Covers both `init_block` and `shell_integration_block`,
        // which are the two places `print_shell_blocks` prints.
        let init = Path::new("/home/\x1b[31mbad/init.zsh");
        let script = Path::new("/home/\x07evil/ghost-complete.zsh");

        let rendered_init = init_block(init);
        let rendered_shell = shell_integration_block(script);

        assert!(
            !rendered_init.contains('\x1b'),
            "init_block must strip ESC: {rendered_init:?}"
        );
        assert!(
            !rendered_shell.contains('\x07'),
            "shell_integration_block must strip BEL: {rendered_shell:?}"
        );
        // Printable surroundings remain (literal `[31m` / `bad` survive).
        assert!(rendered_init.contains("[31mbad/init.zsh"));
        assert!(rendered_shell.contains("evil/ghost-complete.zsh"));
    }

    #[test]
    fn test_init_block_metacharacters_safe() {
        let path = Path::new("/home/$(rm -rf /)/config/ghost-complete/init.zsh");
        let block = init_block(path);
        // Must be single-quoted — no shell expansion possible
        assert!(block.contains("'/home/$(rm -rf /)/config/ghost-complete/init.zsh'"));
        // Must NOT contain the path inside double quotes (would allow expansion)
        assert!(!block.contains("\"$(rm -rf /)\""));
        // The echo line must close double quotes BEFORE the single-quoted path
        assert!(
            block.contains(
                r#"echo "ghost-complete: init script missing: "'/home/$(rm -rf /)/config/ghost-complete/init.zsh'"#
            ),
            "echo line must not embed single-quoted path inside double quotes:\n{}",
            block
        );
    }

    #[test]
    fn test_install_creates_files() {
        let dir = TempDir::new().unwrap();
        let zshrc = dir.path().join(".zshrc");
        let config = dir.path().join("config");

        install_to(&zshrc, &config, false).unwrap();

        // .zshrc should exist with both blocks
        let content = fs::read_to_string(&zshrc).unwrap();
        assert!(content.contains(INIT_BEGIN));
        assert!(content.contains(INIT_END));
        assert!(content.contains(SHELL_BEGIN));
        assert!(content.contains(SHELL_END));
        // Init script should be written and sourced
        let init_script = config.join("shell/init.zsh");
        assert!(init_script.exists());
        let init_content = fs::read_to_string(&init_script).unwrap();
        assert_eq!(init_content, ZSH_INIT);
        let expected_init_source = format!("builtin source {}", shell_safe_path(&init_script));
        assert!(
            content.contains(&expected_init_source),
            "init source path mismatch: .zshrc does not contain '{}'",
            expected_init_source
        );

        // Zsh shell integration script should be written and sourced
        let script = config.join("shell/ghost-complete.zsh");
        assert!(script.exists());
        let script_content = fs::read_to_string(&script).unwrap();
        assert_eq!(script_content, ZSH_INTEGRATION);
        let expected_source = format!("source {}", shell_safe_path(&script));
        assert!(
            content.contains(&expected_source),
            "source path mismatch: .zshrc does not contain '{}'",
            expected_source
        );

        // Bash/fish are not deployed
        assert!(!config.join("shell/ghost-complete.bash").exists());
        assert!(!config.join("shell/ghost-complete.fish").exists());
    }

    #[test]
    fn test_install_no_existing_zshrc() {
        let dir = TempDir::new().unwrap();
        let zshrc = dir.path().join(".zshrc");
        let config = dir.path().join("config");

        // .zshrc doesn't exist yet
        assert!(!zshrc.exists());
        install_to(&zshrc, &config, false).unwrap();

        let content = fs::read_to_string(&zshrc).unwrap();
        assert!(content.contains(INIT_BEGIN));
        assert!(content.contains(SHELL_BEGIN));
    }

    #[test]
    fn test_install_preserves_existing_content() {
        let dir = TempDir::new().unwrap();
        let zshrc = dir.path().join(".zshrc");
        let config = dir.path().join("config");

        let existing = "export PATH=\"/usr/local/bin:$PATH\"\nalias ll='ls -la'\n";
        fs::write(&zshrc, existing).unwrap();

        install_to(&zshrc, &config, false).unwrap();

        let content = fs::read_to_string(&zshrc).unwrap();
        assert!(content.contains("export PATH=\"/usr/local/bin:$PATH\""));
        assert!(content.contains("alias ll='ls -la'"));
        assert!(content.contains(INIT_BEGIN));
        assert!(content.contains(SHELL_BEGIN));

        // Init block should be before user content
        let init_pos = content.find(INIT_BEGIN).unwrap();
        let user_pos = content.find("export PATH").unwrap();
        let shell_pos = content.find(SHELL_BEGIN).unwrap();
        assert!(init_pos < user_pos);
        assert!(user_pos < shell_pos);
    }

    #[test]
    fn test_idempotency() {
        let dir = TempDir::new().unwrap();
        let zshrc = dir.path().join(".zshrc");
        let config = dir.path().join("config");

        let existing = "export FOO=bar\n";
        fs::write(&zshrc, existing).unwrap();

        install_to(&zshrc, &config, false).unwrap();
        let first = fs::read_to_string(&zshrc).unwrap();

        install_to(&zshrc, &config, false).unwrap();
        let second = fs::read_to_string(&zshrc).unwrap();

        assert_eq!(first, second);
    }

    #[test]
    fn test_uninstall_removes_blocks() {
        let dir = TempDir::new().unwrap();
        let zshrc = dir.path().join(".zshrc");
        let config = dir.path().join("config");

        let existing = "export FOO=bar\n";
        fs::write(&zshrc, existing).unwrap();

        // Install then uninstall
        install_to(&zshrc, &config, false).unwrap();
        uninstall_from(&zshrc, &config).unwrap();

        // Blocks should be gone
        let content = fs::read_to_string(&zshrc).unwrap();
        assert!(!content.contains(INIT_BEGIN));
        assert!(!content.contains(SHELL_BEGIN));
        assert!(content.contains("export FOO=bar"));

        // Shell scripts should be removed
        assert!(!config.join("shell/init.zsh").exists());
        assert!(!config.join("shell/ghost-complete.zsh").exists());
    }

    #[test]
    fn test_install_creates_backup() {
        let dir = TempDir::new().unwrap();
        let zshrc = dir.path().join(".zshrc");
        let config = dir.path().join("config");

        let existing = "export ORIGINAL=true\n";
        fs::write(&zshrc, existing).unwrap();

        install_to(&zshrc, &config, false).unwrap();

        // with_extension replaces .zshrc extension
        let backup = zshrc.with_extension("backup.ghost-complete");
        let backup_content = fs::read_to_string(&backup).unwrap();
        assert_eq!(backup_content, existing);
    }

    #[test]
    fn test_install_backup_preserves_source_mode() {
        // Regression for the install.rs TOCTOU fix: when fs::copy was replaced
        // with OpenOptions::create_new(true), mode preservation was silently
        // dropped and a 0o600 source .zshrc would be backed up as 0o644 (or
        // whatever the umask left), exposing shell secrets to other users.
        use std::os::unix::fs::PermissionsExt;

        let dir = TempDir::new().unwrap();
        let zshrc = dir.path().join(".zshrc");
        let config = dir.path().join("config");

        fs::write(&zshrc, "export SECRET_TOKEN=hunter2\n").unwrap();
        // Restrict to owner-only, like a security-conscious user might.
        fs::set_permissions(&zshrc, fs::Permissions::from_mode(0o600)).unwrap();

        install_to(&zshrc, &config, false).unwrap();

        let backup = zshrc.with_extension("backup.ghost-complete");
        let backup_mode = fs::metadata(&backup).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            backup_mode, 0o600,
            "backup must preserve source mode — got {backup_mode:o}, expected 600"
        );
    }

    #[test]
    fn test_install_creates_default_config() {
        let dir = TempDir::new().unwrap();
        let zshrc = dir.path().join(".zshrc");
        let config = dir.path().join("config");

        install_to(&zshrc, &config, false).unwrap();

        let config_path = config.join("config.toml");
        assert!(config_path.exists());
        let content = fs::read_to_string(&config_path).unwrap();
        assert!(content.contains("[keybindings]"));
        assert!(content.contains("[trigger]"));
        assert!(content.contains("[popup]"));
        assert!(content.contains("[theme]"));
        // Should parse as valid TOML config (all theme fields are commented out)
        let parsed: gc_config::GhostConfig = toml::from_str(&content).unwrap();
        assert_eq!(parsed.keybindings.accept, "tab");
        // Commented-out theme overrides leave the fields as None (inherit preset).
        assert_eq!(parsed.theme.selected, None);
        assert_eq!(parsed.theme.description, None);
    }

    #[test]
    fn test_install_does_not_clobber_existing_config() {
        let dir = TempDir::new().unwrap();
        let zshrc = dir.path().join(".zshrc");
        let config = dir.path().join("config");

        fs::create_dir_all(&config).unwrap();
        let config_path = config.join("config.toml");
        let custom = "[keybindings]\naccept = \"enter\"\n";
        fs::write(&config_path, custom).unwrap();

        install_to(&zshrc, &config, false).unwrap();

        let content = fs::read_to_string(&config_path).unwrap();
        assert_eq!(content, custom);
    }

    #[test]
    fn test_copy_embedded_specs() {
        let config_dir = TempDir::new().unwrap();

        copy_specs(config_dir.path()).unwrap();

        let dest = config_dir.path().join("specs");
        assert!(dest.exists());

        // All embedded specs should be written
        for (name, _) in EMBEDDED_SPECS {
            assert!(
                dest.join(name).exists(),
                "expected spec {name} to be installed"
            );
        }
        assert_eq!(
            fs::read_dir(&dest).unwrap().count(),
            EMBEDDED_SPECS.len(),
            "spec count mismatch"
        );
    }

    #[test]
    fn test_install_readonly_zshrc_succeeds() {
        use std::os::unix::fs::PermissionsExt;

        let dir = TempDir::new().unwrap();
        let zshrc = dir.path().join(".zshrc");
        let config = dir.path().join("config");

        // Create a read-only .zshrc
        fs::write(&zshrc, "export FOO=bar\n").unwrap();
        fs::set_permissions(&zshrc, fs::Permissions::from_mode(0o444)).unwrap();

        // Install should succeed (graceful fallback, not error)
        let result = install_to(&zshrc, &config, false);
        assert!(result.is_ok());

        // File deployments should still have happened
        assert!(config.join("shell/init.zsh").exists());
        assert!(config.join("shell/ghost-complete.zsh").exists());
        assert!(config.join("specs").exists());
    }

    #[test]
    fn test_install_dry_run_no_writes() {
        let dir = TempDir::new().unwrap();
        let zshrc = dir.path().join(".zshrc");
        let config = dir.path().join("config");

        install_to(&zshrc, &config, true).unwrap();

        // Nothing should have been created
        assert!(!zshrc.exists());
        assert!(!config.exists());
    }

    #[test]
    fn test_install_dry_run_existing_files_untouched() {
        let dir = TempDir::new().unwrap();
        let zshrc = dir.path().join(".zshrc");
        let config = dir.path().join("config");

        let existing = "export FOO=bar\n";
        fs::write(&zshrc, existing).unwrap();

        fs::create_dir_all(&config).unwrap();
        let config_path = config.join("config.toml");
        let custom_config = "[keybindings]\naccept = \"enter\"\n";
        fs::write(&config_path, custom_config).unwrap();

        install_to(&zshrc, &config, true).unwrap();

        // .zshrc should be unchanged
        assert_eq!(fs::read_to_string(&zshrc).unwrap(), existing);
        // config should be unchanged
        assert_eq!(fs::read_to_string(&config_path).unwrap(), custom_config);
        // No shell scripts should have been created
        assert!(!config.join("shell").exists());
    }

    #[test]
    fn test_backup_not_overwritten_on_reinstall() {
        let dir = TempDir::new().unwrap();
        let zshrc = dir.path().join(".zshrc");
        let config = dir.path().join("config");

        let original = "export ORIGINAL=true\n";
        fs::write(&zshrc, original).unwrap();

        // First install — creates backup with original content
        install_to(&zshrc, &config, false).unwrap();
        let backup = zshrc.with_extension("backup.ghost-complete");
        assert!(backup.exists());
        assert_eq!(fs::read_to_string(&backup).unwrap(), original);

        // Second install — backup should NOT be overwritten
        install_to(&zshrc, &config, false).unwrap();
        assert_eq!(
            fs::read_to_string(&backup).unwrap(),
            original,
            "backup was overwritten on second install — original content lost"
        );
    }

    #[test]
    fn test_uninstall_prints_retained_files_note() {
        let dir = TempDir::new().unwrap();
        let zshrc = dir.path().join(".zshrc");
        let config = dir.path().join("config");

        fs::write(&zshrc, "export FOO=bar\n").unwrap();
        install_to(&zshrc, &config, false).unwrap();

        // After install, specs and config should exist
        assert!(config.join("specs").exists());
        assert!(config.join("config.toml").exists());

        // Uninstall — should succeed and leave specs/config behind
        uninstall_from(&zshrc, &config).unwrap();

        // Specs and config should still be there (retained)
        assert!(config.join("specs").exists());
        assert!(config.join("config.toml").exists());
    }

    #[test]
    fn test_post_install_summary_contains_all_sections() {
        // Use a distinctive directory so we can also assert the helper
        // interpolates `config_dir` into the rendered paths rather than
        // hardcoding them.
        let config_dir = Path::new("/tmp/gc-test-xyz");
        let summary = post_install_summary(config_dir, true);
        for token in [
            "ghost-complete installed successfully",
            "doctor",
            "Ctrl+/",
            "config edit",
            "git",
            "config.toml",
            "specs",
            "source ~/.zshrc",
            "/tmp/gc-test-xyz/config.toml",
            "/tmp/gc-test-xyz/specs",
        ] {
            assert!(
                summary.contains(token),
                "missing token: {token}\n--- summary ---\n{summary}"
            );
        }
        let count_token = format!("({} completion specs)", EMBEDDED_SPECS.len());
        assert!(
            summary.contains(&count_token),
            "missing spec count `{count_token}`\n--- summary ---\n{summary}"
        );
    }

    #[test]
    fn test_post_install_summary_manual_fallback_omits_source_zshrc() {
        let summary = post_install_summary(Path::new("/tmp/cfg"), false);
        assert!(summary.contains("after pasting the blocks above"));
        assert!(
            !summary.contains("source ~/.zshrc"),
            "manual-fallback summary must not instruct user to source a file \
             they didn't write to:\n{summary}"
        );
    }

    #[test]
    fn test_post_install_summary_uses_sanitized_paths() {
        // Pin the sanitization invariant. We can't blanket-assert
        // `!contains('\x1b')` because the helper intentionally emits ANSI
        // sigils (green check, bolds, dim placeholder); instead pin both
        // directions — the path's raw sequence is gone, the sanitised form
        // is present.
        let hostile = Path::new("/tmp/\x1b[31mevil");
        let summary = post_install_summary(hostile, true);
        assert!(
            summary.contains("/tmp/[31mevil"),
            "expected sanitised hostile path in summary: {summary:?}"
        );
        assert!(
            !summary.contains("\x1b[31m"),
            "raw ESC sequence from hostile path leaked: {summary:?}"
        );
    }
}

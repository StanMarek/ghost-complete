use anyhow::{Context, Result};
use std::fs;
use std::path::Path;

const ZSH_INTEGRATION: &str = include_str!("../../../shell/ghost-complete.zsh");
const BASH_INTEGRATION: &str = include_str!("../../../shell/ghost-complete.bash");
const FISH_INTEGRATION: &str = include_str!("../../../shell/ghost-complete.fish");

const EMBEDDED_SPECS: &[(&str, &str)] = &[
    ("brew.json", include_str!("../../../specs/brew.json")),
    ("cargo.json", include_str!("../../../specs/cargo.json")),
    ("cd.json", include_str!("../../../specs/cd.json")),
    ("curl.json", include_str!("../../../specs/curl.json")),
    ("docker.json", include_str!("../../../specs/docker.json")),
    ("gh.json", include_str!("../../../specs/gh.json")),
    ("git.json", include_str!("../../../specs/git.json")),
    ("grep.json", include_str!("../../../specs/grep.json")),
    ("jq.json", include_str!("../../../specs/jq.json")),
    ("kubectl.json", include_str!("../../../specs/kubectl.json")),
    ("make.json", include_str!("../../../specs/make.json")),
    ("npm.json", include_str!("../../../specs/npm.json")),
    ("pip.json", include_str!("../../../specs/pip.json")),
    ("pip3.json", include_str!("../../../specs/pip3.json")),
    ("python.json", include_str!("../../../specs/python.json")),
    ("python3.json", include_str!("../../../specs/python3.json")),
    ("ssh.json", include_str!("../../../specs/ssh.json")),
    ("tar.json", include_str!("../../../specs/tar.json")),
];

const DEFAULT_CONFIG_TOML: &str = "\
# Ghost Complete configuration
# Uncomment and edit values to customize. All values shown are defaults.

# [trigger]
# auto_chars = [' ', '/', '-', '.']
# delay_ms = 150

# [popup]
# max_visible = 10
# min_width = 20
# max_width = 60

# [suggest]
# max_results = 50
# max_history_entries = 10000

# [suggest.providers]
# commands = true
# history = true
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
selected = \"reverse\"
description = \"dim\"
";

const INIT_BEGIN: &str = "# >>> ghost-complete initialize >>>";
const INIT_END: &str = "# <<< ghost-complete initialize <<<";
const SHELL_BEGIN: &str = "# >>> ghost-complete shell integration >>>";
const SHELL_END: &str = "# <<< ghost-complete shell integration <<<";
const MANAGED_WARNING: &str =
    "# !! Contents within this block are managed by 'ghost-complete install' !!";

fn init_block() -> String {
    format!(
        "{INIT_BEGIN}\n\
         {MANAGED_WARNING}\n\
         if [[ \"$TERM_PROGRAM\" == \"ghostty\" && -z \"$GHOST_COMPLETE_ACTIVE\" ]]; then\n  \
           export GHOST_COMPLETE_ACTIVE=1\n  \
           exec ghost-complete\n\
         fi\n\
         {INIT_END}"
    )
}

fn shell_integration_block(script_path: &Path) -> String {
    format!(
        "{SHELL_BEGIN}\n\
         {MANAGED_WARNING}\n\
         source \"{}\"\n\
         {SHELL_END}",
        script_path.display()
    )
}

/// Strips a managed block delimited by `begin`..`end` markers from `content`.
/// Returns `(new_content, was_found)`.
fn remove_block(content: &str, begin: &str, end: &str) -> (String, bool) {
    let Some(start_idx) = content.find(begin) else {
        return (content.to_string(), false);
    };
    let Some(end_match) = content[start_idx..].find(end) else {
        return (content.to_string(), false);
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

    (result, true)
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
    println!("  Installed {count} completion specs to {}", dest.display());
    Ok(())
}

fn install_to(zshrc_path: &Path, config_dir: &Path) -> Result<()> {
    // 1. Write shell integration script
    let shell_dir = config_dir.join("shell");
    fs::create_dir_all(&shell_dir)
        .with_context(|| format!("failed to create {}", shell_dir.display()))?;

    let script_path = shell_dir.join("ghost-complete.zsh");
    fs::write(&script_path, ZSH_INTEGRATION)
        .with_context(|| format!("failed to write {}", script_path.display()))?;
    println!("  Wrote zsh integration to {}", script_path.display());

    let bash_path = shell_dir.join("ghost-complete.bash");
    fs::write(&bash_path, BASH_INTEGRATION)
        .with_context(|| format!("failed to write {}", bash_path.display()))?;
    println!("  Wrote bash integration to {}", bash_path.display());

    let fish_path = shell_dir.join("ghost-complete.fish");
    fs::write(&fish_path, FISH_INTEGRATION)
        .with_context(|| format!("failed to write {}", fish_path.display()))?;
    println!("  Wrote fish integration to {}", fish_path.display());

    // 1b. Copy completion specs
    copy_specs(config_dir)?;

    // 1c. Write default config.toml if one doesn't exist (never clobber)
    let config_path = config_dir.join("config.toml");
    if !config_path.exists() {
        fs::write(&config_path, DEFAULT_CONFIG_TOML)
            .with_context(|| format!("failed to write {}", config_path.display()))?;
        println!("  Wrote default config to {}", config_path.display());
    } else {
        println!("  Config already exists at {}", config_path.display());
    }

    // 2. Read existing .zshrc (or empty)
    let existing = if zshrc_path.exists() {
        fs::read_to_string(zshrc_path)
            .with_context(|| format!("failed to read {}", zshrc_path.display()))?
    } else {
        String::new()
    };

    // 3. Backup
    if zshrc_path.exists() {
        let backup = zshrc_path.with_extension("backup.ghost-complete");
        fs::copy(zshrc_path, &backup)
            .with_context(|| format!("failed to backup to {}", backup.display()))?;
        println!("  Backed up .zshrc to {}", backup.display());
    }

    // 4. Strip existing managed blocks (idempotent)
    let (content, _) = remove_block(&existing, INIT_BEGIN, INIT_END);
    let (content, _) = remove_block(&content, SHELL_BEGIN, SHELL_END);

    // 5. Prepend init block, append shell integration block
    let content = content.trim().to_string();
    let mut new_zshrc = String::new();
    new_zshrc.push_str(&init_block());
    new_zshrc.push('\n');
    if !content.is_empty() {
        new_zshrc.push_str(&content);
        new_zshrc.push('\n');
    }
    new_zshrc.push_str(&shell_integration_block(&script_path));
    new_zshrc.push('\n');

    // 6. Write
    fs::write(zshrc_path, &new_zshrc)
        .with_context(|| format!("failed to write {}", zshrc_path.display()))?;
    println!("  Updated {}", zshrc_path.display());

    println!("\nghost-complete installed successfully!");
    println!("Restart your shell or run: source ~/.zshrc");
    println!();
    println!(
        "For bash: Add to .bashrc:    source {}",
        bash_path.display()
    );
    println!(
        "For fish: Add to config.fish: source {}",
        fish_path.display()
    );
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
            println!("  Removed managed blocks from {}", zshrc_path.display());
        } else {
            println!(
                "  No ghost-complete blocks found in {}",
                zshrc_path.display()
            );
        }
    } else {
        println!("  {} does not exist, nothing to do", zshrc_path.display());
    }

    // 2. Remove shell integration scripts
    for name in &[
        "ghost-complete.zsh",
        "ghost-complete.bash",
        "ghost-complete.fish",
    ] {
        let script_path = config_dir.join("shell").join(name);
        if script_path.exists() {
            fs::remove_file(&script_path)
                .with_context(|| format!("failed to remove {}", script_path.display()))?;
            println!("  Removed {}", script_path.display());
        }
    }

    // 3. Clean up empty shell/ directory (best-effort)
    let shell_dir = config_dir.join("shell");
    if shell_dir.exists() {
        let _ = fs::remove_dir(&shell_dir); // only succeeds if empty
    }

    println!("\nghost-complete uninstalled successfully!");
    Ok(())
}

pub fn run_install() -> Result<()> {
    let home = dirs::home_dir().context("could not determine home directory")?;
    let zshrc = home.join(".zshrc");
    let config_dir = gc_config::config_dir().context("could not determine home directory")?;

    println!("Installing ghost-complete...\n");
    install_to(&zshrc, &config_dir)
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
        let block = init_block();
        assert!(block.contains(INIT_BEGIN));
        assert!(block.contains(INIT_END));
        assert!(block.contains(MANAGED_WARNING));
        assert!(block.contains("GHOST_COMPLETE_ACTIVE"));
        assert!(block.contains("exec ghost-complete"));
    }

    #[test]
    fn test_shell_integration_block_content() {
        let path = Path::new("/some/path/ghost-complete.zsh");
        let block = shell_integration_block(path);
        assert!(block.contains(SHELL_BEGIN));
        assert!(block.contains(SHELL_END));
        assert!(block.contains(MANAGED_WARNING));
        assert!(block.contains("/some/path/ghost-complete.zsh"));
    }

    #[test]
    fn test_install_creates_files() {
        let dir = TempDir::new().unwrap();
        let zshrc = dir.path().join(".zshrc");
        let config = dir.path().join("config");

        install_to(&zshrc, &config).unwrap();

        // .zshrc should exist with both blocks
        let content = fs::read_to_string(&zshrc).unwrap();
        assert!(content.contains(INIT_BEGIN));
        assert!(content.contains(INIT_END));
        assert!(content.contains(SHELL_BEGIN));
        assert!(content.contains(SHELL_END));
        assert!(content.contains("GHOST_COMPLETE_ACTIVE"));

        // Shell scripts should be written
        let script = config.join("shell/ghost-complete.zsh");
        assert!(script.exists());
        let script_content = fs::read_to_string(&script).unwrap();
        assert_eq!(script_content, ZSH_INTEGRATION);

        let bash_script = config.join("shell/ghost-complete.bash");
        assert!(bash_script.exists());
        let bash_content = fs::read_to_string(&bash_script).unwrap();
        assert_eq!(bash_content, BASH_INTEGRATION);

        let fish_script = config.join("shell/ghost-complete.fish");
        assert!(fish_script.exists());
        let fish_content = fs::read_to_string(&fish_script).unwrap();
        assert_eq!(fish_content, FISH_INTEGRATION);

        // Source path in .zshrc must match actual script location
        let expected_source = format!("source \"{}\"", script.display());
        assert!(
            content.contains(&expected_source),
            "source path mismatch: .zshrc does not contain '{}'",
            expected_source
        );
    }

    #[test]
    fn test_install_no_existing_zshrc() {
        let dir = TempDir::new().unwrap();
        let zshrc = dir.path().join(".zshrc");
        let config = dir.path().join("config");

        // .zshrc doesn't exist yet
        assert!(!zshrc.exists());
        install_to(&zshrc, &config).unwrap();

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

        install_to(&zshrc, &config).unwrap();

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

        install_to(&zshrc, &config).unwrap();
        let first = fs::read_to_string(&zshrc).unwrap();

        install_to(&zshrc, &config).unwrap();
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
        install_to(&zshrc, &config).unwrap();
        uninstall_from(&zshrc, &config).unwrap();

        // Blocks should be gone
        let content = fs::read_to_string(&zshrc).unwrap();
        assert!(!content.contains(INIT_BEGIN));
        assert!(!content.contains(SHELL_BEGIN));
        assert!(content.contains("export FOO=bar"));

        // Scripts should be removed
        assert!(!config.join("shell/ghost-complete.zsh").exists());
        assert!(!config.join("shell/ghost-complete.bash").exists());
        assert!(!config.join("shell/ghost-complete.fish").exists());
    }

    #[test]
    fn test_install_creates_backup() {
        let dir = TempDir::new().unwrap();
        let zshrc = dir.path().join(".zshrc");
        let config = dir.path().join("config");

        let existing = "export ORIGINAL=true\n";
        fs::write(&zshrc, existing).unwrap();

        install_to(&zshrc, &config).unwrap();

        // with_extension replaces .zshrc extension
        let backup = zshrc.with_extension("backup.ghost-complete");
        let backup_content = fs::read_to_string(&backup).unwrap();
        assert_eq!(backup_content, existing);
    }

    #[test]
    fn test_install_creates_default_config() {
        let dir = TempDir::new().unwrap();
        let zshrc = dir.path().join(".zshrc");
        let config = dir.path().join("config");

        install_to(&zshrc, &config).unwrap();

        let config_path = config.join("config.toml");
        assert!(config_path.exists());
        let content = fs::read_to_string(&config_path).unwrap();
        assert!(content.contains("[keybindings]"));
        assert!(content.contains("[trigger]"));
        assert!(content.contains("[popup]"));
        assert!(content.contains("[theme]"));
        // Should parse as valid TOML config with theme defaults active
        let parsed: gc_config::GhostConfig = toml::from_str(&content).unwrap();
        assert_eq!(parsed.keybindings.accept, "tab");
        assert_eq!(parsed.theme.selected, "reverse");
        assert_eq!(parsed.theme.description, "dim");
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

        install_to(&zshrc, &config).unwrap();

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
}

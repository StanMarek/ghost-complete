//! `init.zsh` loads the hooks script (`ghost-complete.zsh`) at the first
//! prompt of every shell behind the proxy, so `.zshrc` needs no second block
//! to source it (#186). The hooks report prompts, the working directory and
//! the command line to the proxy; a shell without a proxy must not load them.
//!
//! Runs the real `init.zsh` and `ghost-complete.zsh` in an interactive zsh
//! with stdin piped: zsh runs its precmd hooks before it reads each command,
//! so every line on stdin starts a new prompt.

use std::path::Path;
use std::process::{Command, Stdio};

#[derive(Clone, Copy)]
enum Proxy {
    /// Inside tmux, in the pane the proxy runs in.
    TmuxPane,
    /// Outside tmux, the shell's parent is the proxy.
    Parent,
    /// No proxy: a terminal Ghost Complete doesn't support.
    None,
}

struct Setup<'a> {
    proxy: Proxy,
    /// `.zshrc` lines before `init.zsh` is sourced.
    before_init: &'a str,
    /// `.zshrc` lines after it; `SHELL_DIR` is replaced with the scripts'
    /// directory.
    after_init: &'a str,
    /// The command typed at the first prompt.
    first_command: &'a str,
    /// Whether `ghost-complete.zsh` is installed next to `init.zsh`.
    hooks_script: bool,
    /// Whether `init.zsh` is a symlink to a copy in another directory, as
    /// a dotfiles manager would leave it.
    init_symlinked: bool,
}

impl Default for Setup<'_> {
    fn default() -> Self {
        Setup {
            proxy: Proxy::TmuxPane,
            before_init: "",
            after_init: "",
            first_command: ":",
            hooks_script: true,
            init_symlinked: false,
        }
    }
}

/// The prompt mark the hooks send on a terminal without native OSC 133.
const MARK: &str = "\x1b]7771;A\x07";
/// The working-directory report the hooks send at every prompt.
const OSC7: &str = "\x1b]7;file://";

const STATE: &str = r#"print -r -- "STATE precmd=${(j:,:)precmd_functions} preexec=${(j:,:)preexec_functions} widget=${widgets[zle-line-pre-redraw]-none} budget=${_GC_ENV_TOTAL_BUDGET-unset}""#;

/// A user widget that wraps `zle-line-pre-redraw` after the hooks loaded.
const USER_WRAPPER: &str = r#"_user_redraw() { zle _user_orig_redraw -- "$@"; }
zle -N _user_orig_redraw ${widgets[zle-line-pre-redraw]#user:}
zle -N zle-line-pre-redraw _user_redraw"#;

struct Run {
    /// stdout of each prompt's precmd hooks, in order.
    prompts: Vec<String>,
    state: String,
    stderr: String,
}

fn run(setup: Setup) -> Run {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let shell_dir = home.join("shell");
    std::fs::create_dir(&shell_dir).unwrap();
    let repo_shell = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../shell");
    if setup.init_symlinked {
        let dotfiles = home.join("dotfiles");
        std::fs::create_dir(&dotfiles).unwrap();
        std::fs::copy(repo_shell.join("init.zsh"), dotfiles.join("init.zsh")).unwrap();
        std::os::unix::fs::symlink(dotfiles.join("init.zsh"), shell_dir.join("init.zsh")).unwrap();
    } else {
        std::fs::copy(repo_shell.join("init.zsh"), shell_dir.join("init.zsh")).unwrap();
    }
    if setup.hooks_script {
        std::fs::copy(
            repo_shell.join("ghost-complete.zsh"),
            shell_dir.join("ghost-complete.zsh"),
        )
        .unwrap();
    }
    let shell_dir = shell_dir.to_str().unwrap();
    // Keep /etc/zshrc out: macOS's adds hooks of its own.
    std::fs::write(home.join(".zshenv"), "unsetopt global_rcs\n").unwrap();
    std::fs::write(
        home.join(".zshrc"),
        format!(
            "PS1=''\n{}\nsource '{shell_dir}/init.zsh'\n{}\n",
            setup.before_init,
            setup.after_init.replace("SHELL_DIR", shell_dir),
        ),
    )
    .unwrap();
    let stdin = [setup.first_command, ":", STATE]
        .iter()
        .map(|command| format!("print -r -- '<cmd>'; {command}\n"))
        .collect::<String>()
        + "exit\n";
    std::fs::write(home.join("stdin"), stdin).unwrap();

    let mut cmd = match setup.proxy {
        Proxy::Parent => {
            let mut cmd = Command::new("/bin/zsh");
            cmd.arg("-f")
                .arg("-c")
                .arg(r#"exec -a ghost-complete /bin/zsh -f -c '/bin/zsh -i; exit $?'"#);
            cmd
        }
        _ => {
            let mut cmd = Command::new("/bin/zsh");
            cmd.arg("-i");
            cmd
        }
    };
    cmd.env_clear()
        .env("HOME", home)
        .env("ZDOTDIR", home)
        .env("PATH", "/usr/bin:/bin")
        .env("TERM", "dumb")
        .stdin(Stdio::from(
            std::fs::File::open(home.join("stdin")).unwrap(),
        ));
    match setup.proxy {
        Proxy::TmuxPane => {
            cmd.env("TMUX", "/tmp/tmux-test/default,1,0")
                .env("TMUX_PANE", "%1")
                .env("GHOST_COMPLETE_PANE", "%1")
                .env("GHOST_COMPLETE_ACTIVE", "1");
        }
        Proxy::Parent => {
            cmd.env("GHOST_COMPLETE_ACTIVE", "1");
        }
        Proxy::None => {
            cmd.env("TERM_PROGRAM", "unsupported");
        }
    }
    let output = cmd.output().expect("run zsh");
    let stdout = String::from_utf8(output.stdout).unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(output.status.success(), "zsh failed: {stderr}");

    let state = stdout
        .lines()
        .find_map(|line| line.strip_prefix("STATE "))
        .unwrap_or_else(|| panic!("no state line in {stdout:?}\nstderr: {stderr}"))
        .to_owned();
    let prompts: Vec<String> = stdout.split("<cmd>\n").map(str::to_owned).collect();
    assert_eq!(prompts.len(), 4, "expected 4 prompts in {stdout:?}");
    Run {
        prompts,
        state,
        stderr,
    }
}

fn assert_hooks_ran_once_per_prompt(run: &Run) {
    for (i, prompt) in run.prompts.iter().enumerate() {
        assert_eq!(
            prompt.matches(MARK).count(),
            1,
            "prompt {i} mark count: {prompt:?}\nstderr: {}",
            run.stderr
        );
        assert_eq!(
            prompt.matches(OSC7).count(),
            1,
            "prompt {i} OSC 7 count: {prompt:?}\nstderr: {}",
            run.stderr
        );
    }
}

fn assert_no_hooks(run: &Run) {
    for (i, prompt) in run.prompts.iter().enumerate() {
        assert!(
            !prompt.contains(MARK) && !prompt.contains(OSC7),
            "prompt {i} has hook output: {prompt:?}"
        );
    }
    assert!(
        run.state
            .starts_with("precmd= preexec= widget=none budget=unset"),
        "hooks registered: {}",
        run.state
    );
}

const LOADED: &str =
    "precmd=_gc_precmd,_gc_osc7 preexec=_gc_preexec widget=user:_gc_report_buffer budget=524288";

#[test]
fn hooks_load_at_the_first_prompt_in_the_proxy_pane() {
    let run = run(Setup::default());
    assert_hooks_ran_once_per_prompt(&run);
    assert_eq!(run.state, LOADED);
}

#[test]
fn hooks_load_at_the_first_prompt_under_the_proxy() {
    let run = run(Setup {
        proxy: Proxy::Parent,
        ..Setup::default()
    });
    assert_hooks_ran_once_per_prompt(&run);
    assert_eq!(run.state, LOADED);
}

#[test]
fn hooks_a_zshrc_already_loaded_are_left_alone() {
    // A .zshrc from before init.zsh loaded the hooks still sources them at
    // the bottom, and a plugin loaded after it wraps the zle widget. Loading
    // the hooks again would wrap the plugin's widget, and the two wrappers
    // would call each other.
    let after_init = format!("source 'SHELL_DIR/ghost-complete.zsh'\n{USER_WRAPPER}");
    let run = run(Setup {
        after_init: &after_init,
        ..Setup::default()
    });
    assert_hooks_ran_once_per_prompt(&run);
    assert_eq!(
        run.state,
        "precmd=_gc_precmd,_gc_osc7 preexec=_gc_preexec widget=user:_user_redraw budget=524288"
    );
}

#[test]
fn sourcing_zshrc_again_loads_nothing_twice() {
    let run = run(Setup {
        first_command: "source $ZDOTDIR/.zshrc",
        ..Setup::default()
    });
    assert_hooks_ran_once_per_prompt(&run);
    assert_eq!(run.state, LOADED);
}

#[test]
fn symlinked_init_zsh_finds_the_hooks_script_beside_the_link() {
    // install, doctor and .zshrc all name the file by the link's path;
    // the hooks script sits next to the link, not next to its target.
    let run = run(Setup {
        init_symlinked: true,
        ..Setup::default()
    });
    assert_hooks_ran_once_per_prompt(&run);
    assert_eq!(run.state, LOADED);
}

#[test]
fn missing_hooks_script_warns_once() {
    let run = run(Setup {
        hooks_script: false,
        ..Setup::default()
    });
    assert_eq!(
        run.stderr
            .matches("ghost-complete: hooks script missing: ")
            .count(),
        1,
        "stderr: {}",
        run.stderr
    );
    assert!(
        run.stderr
            .contains("ghost-complete: run 'ghost-complete install' to restore it"),
        "stderr: {}",
        run.stderr
    );
    assert_no_hooks(&run);
}

#[test]
fn shell_without_proxy_loads_no_hooks() {
    let run = run(Setup {
        proxy: Proxy::None,
        ..Setup::default()
    });
    assert_no_hooks(&run);
}

#[test]
fn fallback_shell_loads_no_hooks() {
    // The proxy failed to start and ran the shell in its place: nothing
    // reads the hooks' output.
    let run = run(Setup {
        proxy: Proxy::Parent,
        before_init: "export GHOST_COMPLETE_FALLBACK_PID=$$",
        ..Setup::default()
    });
    assert_no_hooks(&run);
}

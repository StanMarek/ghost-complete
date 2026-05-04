//! Phase 5 (UX-9) integration tests: `script_function`, `custom`, and
//! the Fig host API surface.
//!
//! Each test spins up its own [`gc_jsrt::JsWorker`] so a stuck worker
//! cannot bleed into a sibling test. The worker is cheap to spawn —
//! tests still complete in well under a second on a debug build.
//!
//! Phase 5 contracts under test:
//!
//! - `script_function`: JS evaluates with `(tokens, ctx)` and returns
//!   either an argv array or `{command, args}`. The runtime exposes
//!   the resolved argv via `JsRuntimeOutput.argv`.
//! - `custom`: JS evaluates with `(tokens, executeShellCommand, ctx)`
//!   and returns suggestions directly. The runtime mirrors any
//!   `executeShellCommand` calls into the supplied [`ShellRunner`].
//! - The host API surface: `cwd`, `env`, `currentToken`,
//!   `previousToken`, `tokens`, `searchTerm` are reachable both as
//!   top-level globals and via `__ghost.*` / `fig.*` aliases.
//! - Diagnostic codes: `ShellCommandStringDenied`,
//!   `ShellCommandLimitExceeded`, `ShellCommandFailed`, `InvalidArgv`,
//!   `UnsupportedHostApi` surface as `JsDiagnostic` entries on the
//!   evaluation output.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

use gc_jsrt::{
    JsDiagnosticCode, JsExecutionKind, JsRuntimeInput, JsWorker, ShellRunError, ShellRunOutput,
    ShellRunner, MAX_SHELL_CALLS_PER_EVALUATION,
};

const FAST_TIMEOUT: Duration = Duration::from_millis(1_500);

/// In-memory runner that returns a canned response for whichever argv
/// pattern the test wants to exercise. Counts every call so tests can
/// observe the recursion cap.
struct MockShellRunner {
    /// (argv_join, response) pairs. The argv key is `argv.join(' ')`.
    responses: Vec<(String, Result<ShellRunOutput, ShellRunError>)>,
    /// Total `run_argv` invocations.
    call_count: AtomicUsize,
    /// True if the runner should accept shell-string commands.
    accept_strings: bool,
}

impl MockShellRunner {
    fn new(responses: Vec<(&str, Result<ShellRunOutput, ShellRunError>)>) -> Self {
        Self {
            responses: responses
                .into_iter()
                .map(|(k, v)| (k.to_string(), v))
                .collect(),
            call_count: AtomicUsize::new(0),
            accept_strings: false,
        }
    }

    fn with_string_support(mut self) -> Self {
        self.accept_strings = true;
        self
    }

    fn into_arc(self) -> Arc<dyn ShellRunner> {
        Arc::new(self)
    }
}

impl ShellRunner for MockShellRunner {
    fn run_argv(
        &self,
        argv: &[String],
        _cwd: &Path,
        _timeout: Duration,
    ) -> Result<ShellRunOutput, ShellRunError> {
        self.call_count.fetch_add(1, Ordering::SeqCst);
        let key = argv.join(" ");
        for (pattern, resp) in &self.responses {
            if pattern == &key {
                return match resp {
                    Ok(o) => Ok(o.clone()),
                    Err(e) => Err(e.clone()),
                };
            }
        }
        Err(ShellRunError::Spawn(format!(
            "no canned response for {key:?}"
        )))
    }

    fn run_string(
        &self,
        command: &str,
        cwd: &Path,
        timeout: Duration,
    ) -> Result<ShellRunOutput, ShellRunError> {
        if !self.accept_strings {
            return Err(ShellRunError::StringDenied);
        }
        // Trivial split for tests — production runner uses shlex.
        let argv: Vec<String> = command.split_whitespace().map(str::to_string).collect();
        self.run_argv(&argv, cwd, timeout)
    }
}

#[derive(Default)]
struct RecordingShellRunner {
    cwd_calls: Mutex<Vec<PathBuf>>,
    timeout_calls: Mutex<Vec<Duration>>,
}

impl RecordingShellRunner {
    fn into_arc(self) -> Arc<Self> {
        Arc::new(self)
    }

    fn cwd_calls(&self) -> Vec<PathBuf> {
        self.cwd_calls.lock().expect("cwd calls lock").clone()
    }

    fn timeout_calls(&self) -> Vec<Duration> {
        self.timeout_calls
            .lock()
            .expect("timeout calls lock")
            .clone()
    }
}

impl ShellRunner for RecordingShellRunner {
    fn run_argv(
        &self,
        _argv: &[String],
        cwd: &Path,
        timeout: Duration,
    ) -> Result<ShellRunOutput, ShellRunError> {
        self.cwd_calls
            .lock()
            .expect("cwd calls lock")
            .push(cwd.to_path_buf());
        self.timeout_calls
            .lock()
            .expect("timeout calls lock")
            .push(timeout);
        Ok(ShellRunOutput {
            stdout: "recorded\n".into(),
            stderr: "stderr\n".into(),
            exit_code: Some(0),
        })
    }
}

fn input_with_kind(kind: JsExecutionKind) -> JsRuntimeInput {
    JsRuntimeInput {
        generator_id: "phase5-test".into(),
        kind,
        cwd: PathBuf::from("/phase5"),
        tokens: vec!["git".into(), "checkout".into()],
        current_token: "ma".into(),
        previous_token: "checkout".into(),
        env: {
            let mut m = BTreeMap::new();
            m.insert("HOME".into(), "/home/test".into());
            m.insert("PATH".into(), "/usr/bin".into());
            m
        },
        ..JsRuntimeInput::default()
    }
}

#[tokio::test]
async fn script_function_returns_argv_array() {
    let worker = JsWorker::spawn().expect("spawn");
    // The wrapper synthesised by gc-suggest passes `(tokens, ctx)`
    // explicitly. The worker itself accepts a top-level expression,
    // so we encode the same shape inline here.
    let program = "(function() { \
        const fn = (tokens, ctx) => ['echo', tokens[0], ctx.currentToken]; \
        return fn(tokens, { currentToken: currentToken }); \
    })()";
    let out = worker
        .evaluate(
            program,
            input_with_kind(JsExecutionKind::ScriptFunction),
            FAST_TIMEOUT,
        )
        .await
        .expect("evaluation infra ok");
    assert_eq!(
        out.argv,
        vec!["echo".to_string(), "git".to_string(), "ma".to_string()],
        "script_function argv should preserve token order; got diagnostics={:?}",
        out.diagnostics
    );
    assert!(
        out.suggestions.is_empty(),
        "script_function never produces suggestions directly"
    );
}

#[tokio::test]
async fn script_function_accepts_structured_descriptor() {
    let worker = JsWorker::spawn().expect("spawn");
    let program = "({ command: 'echo', args: ['hello'] })";
    let out = worker
        .evaluate(
            program,
            input_with_kind(JsExecutionKind::ScriptFunction),
            FAST_TIMEOUT,
        )
        .await
        .expect("evaluation infra ok");
    assert_eq!(out.argv, vec!["echo".to_string(), "hello".to_string()]);
}

#[tokio::test]
async fn script_function_invalid_argv_diagnostic() {
    let worker = JsWorker::spawn().expect("spawn");
    // Returning a number is invalid — no argv path can interpret it.
    let out = worker
        .evaluate(
            "42",
            input_with_kind(JsExecutionKind::ScriptFunction),
            FAST_TIMEOUT,
        )
        .await
        .expect("evaluation infra ok");
    assert!(out.argv.is_empty());
    assert!(
        out.diagnostics
            .iter()
            .any(|d| d.code == JsDiagnosticCode::InvalidArgv),
        "expected InvalidArgv, got {:?}",
        out.diagnostics
    );
}

#[tokio::test]
async fn script_function_rejects_non_string_structured_args() {
    let worker = JsWorker::spawn().expect("spawn");
    let program = "({ command: 'echo', args: ['ok', 42] })";
    let out = worker
        .evaluate(
            program,
            input_with_kind(JsExecutionKind::ScriptFunction),
            FAST_TIMEOUT,
        )
        .await
        .expect("evaluation infra ok");
    assert!(out.argv.is_empty());
    assert!(
        out.diagnostics
            .iter()
            .any(|d| d.code == JsDiagnosticCode::InvalidArgv),
        "expected InvalidArgv, got {:?}",
        out.diagnostics
    );
}

#[tokio::test]
async fn custom_calls_execute_shell_command_argv_form() {
    let worker = JsWorker::spawn().expect("spawn");
    let runner = MockShellRunner::new(vec![(
        "echo hello",
        Ok(ShellRunOutput {
            stdout: "hello\nworld\n".into(),
            stderr: String::new(),
            exit_code: Some(0),
        }),
    )])
    .into_arc();
    let mut input = input_with_kind(JsExecutionKind::Custom);
    input.shell_runner = Some(runner);

    // The Custom contract is `async (tokens, exec, ctx) => suggestions`,
    // but the worker evaluates a top-level expression. We synthesise
    // the wrapper inline here.
    let program = "(async () => { \
        const { stdout } = await executeShellCommand(['echo', 'hello']); \
        return stdout.split('\\n').filter(Boolean).map(name => ({ name })); \
    })()";
    let out = worker
        .evaluate(program, input, FAST_TIMEOUT)
        .await
        .expect("infra");
    let names: Vec<&str> = out.suggestions.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(
        names,
        vec!["hello", "world"],
        "diagnostics: {:?}",
        out.diagnostics
    );
}

#[tokio::test]
async fn custom_shell_string_denied_by_default() {
    let worker = JsWorker::spawn().expect("spawn");
    let runner = MockShellRunner::new(Vec::new()).into_arc();
    let mut input = input_with_kind(JsExecutionKind::Custom);
    input.shell_runner = Some(runner);
    input.allow_shell_command = false;

    let program = "(async () => { \
        try { \
            await executeShellCommand('echo hello'); \
            return [{ name: 'should-not-appear' }]; \
        } catch (e) { \
            return [{ name: 'caught:' + e.code }]; \
        } \
    })()";
    let out = worker
        .evaluate(program, input, FAST_TIMEOUT)
        .await
        .expect("infra");
    assert_eq!(
        out.suggestions[0].name, "caught:ShellCommandStringDenied",
        "diagnostics: {:?}",
        out.diagnostics,
    );
}

#[tokio::test]
async fn custom_shell_string_allowed_when_flagged() {
    let worker = JsWorker::spawn().expect("spawn");
    let runner = MockShellRunner::new(vec![(
        "echo hello",
        Ok(ShellRunOutput {
            stdout: "ok\n".into(),
            stderr: String::new(),
            exit_code: Some(0),
        }),
    )])
    .with_string_support()
    .into_arc();
    let mut input = input_with_kind(JsExecutionKind::Custom);
    input.shell_runner = Some(runner);
    input.allow_shell_command = true;

    let program = "(async () => { \
        const { stdout } = await executeShellCommand('echo hello'); \
        return [{ name: stdout.trim() }]; \
    })()";
    let out = worker
        .evaluate(program, input, FAST_TIMEOUT)
        .await
        .expect("infra");
    assert_eq!(
        out.suggestions[0].name, "ok",
        "diagnostics: {:?}",
        out.diagnostics
    );
}

#[tokio::test]
async fn custom_execute_shell_command_recursion_cap_enforced() {
    let worker = JsWorker::spawn().expect("spawn");
    let runner = MockShellRunner::new(vec![(
        "echo x",
        Ok(ShellRunOutput {
            stdout: "x\n".into(),
            stderr: String::new(),
            exit_code: Some(0),
        }),
    )])
    .into_arc();
    let mut input = input_with_kind(JsExecutionKind::Custom);
    input.shell_runner = Some(runner);

    let total_calls = MAX_SHELL_CALLS_PER_EVALUATION + 1;
    let program = format!(
        "(async () => {{ \
            const tags = []; \
            for (let i = 0; i < {total_calls}; i++) {{ \
                try {{ \
                    await executeShellCommand(['echo', 'x']); \
                    tags.push('ok'); \
                }} catch (e) {{ \
                    tags.push('err:' + e.code); \
                    break; \
                }} \
            }} \
            return tags.map(t => ({{ name: t }})); \
        }})()"
    );
    let out = worker
        .evaluate(program, input, FAST_TIMEOUT)
        .await
        .expect("infra");
    let names: Vec<&str> = out.suggestions.iter().map(|s| s.name.as_str()).collect();
    let mut expected: Vec<&str> = vec!["ok"; MAX_SHELL_CALLS_PER_EVALUATION];
    expected.push("err:ShellCommandLimitExceeded");
    assert_eq!(names, expected, "diagnostics: {:?}", out.diagnostics);
}

#[tokio::test]
async fn custom_execute_shell_command_returns_fig_result_object() {
    let worker = JsWorker::spawn().expect("spawn");
    let runner = MockShellRunner::new(vec![(
        "echo hello",
        Ok(ShellRunOutput {
            stdout: "hello\n".into(),
            stderr: "note\n".into(),
            exit_code: Some(7),
        }),
    )])
    .into_arc();
    let mut input = input_with_kind(JsExecutionKind::Custom);
    input.shell_runner = Some(runner);

    let program = "(async () => { \
        const { stdout, stderr, exitCode } = await executeShellCommand({ command: 'echo', args: ['hello'] }); \
        return [{ name: stdout.trim() + ':' + stderr.trim() + ':' + exitCode }]; \
    })()";
    let out = worker
        .evaluate(program, input, FAST_TIMEOUT)
        .await
        .expect("infra");
    assert_eq!(
        out.suggestions[0].name, "hello:note:7",
        "diagnostics: {:?}",
        out.diagnostics
    );
}

#[tokio::test]
async fn custom_execute_shell_command_uses_input_cwd_by_default() {
    let worker = JsWorker::spawn().expect("spawn");
    let runner = RecordingShellRunner::default().into_arc();
    let mut input = input_with_kind(JsExecutionKind::Custom);
    let expected_cwd = std::env::temp_dir().join("gc-jsrt-default-cwd");
    input.cwd = expected_cwd.clone();
    input.shell_runner = Some(runner.clone());

    let out = worker
        .evaluate(
            "(async () => { await executeShellCommand(['pwd-probe']); return [{ name: 'ok' }]; })()",
            input,
            FAST_TIMEOUT,
        )
        .await
        .expect("infra");
    assert_eq!(out.suggestions[0].name, "ok");
    assert_eq!(runner.cwd_calls(), vec![expected_cwd]);
}

#[tokio::test]
async fn custom_execute_shell_command_cwd_options_override_input_cwd() {
    let worker = JsWorker::spawn().expect("spawn");
    let runner = RecordingShellRunner::default().into_arc();
    let mut input = input_with_kind(JsExecutionKind::Custom);
    input.cwd = PathBuf::from("/default");
    input.shell_runner = Some(runner.clone());

    let out = worker
        .evaluate(
            "(async () => { \
                await executeShellCommand(['pwd-probe'], { cwd: '/expected-options' }); \
                await executeShellCommand({ command: 'pwd-probe', cwd: '/expected-descriptor' }); \
                return [{ name: 'ok' }]; \
            })()",
            input,
            FAST_TIMEOUT,
        )
        .await
        .expect("infra");
    assert_eq!(out.suggestions[0].name, "ok");
    assert_eq!(
        runner.cwd_calls(),
        vec![
            PathBuf::from("/expected-options"),
            PathBuf::from("/expected-descriptor"),
        ]
    );
}

#[tokio::test]
async fn custom_execute_shell_command_timeout_is_clamped_to_js_budget() {
    let worker = JsWorker::spawn().expect("spawn");
    let runner = RecordingShellRunner::default().into_arc();
    let mut input = input_with_kind(JsExecutionKind::Custom);
    input.shell_runner = Some(runner.clone());

    let out = worker
        .evaluate(
            "(async () => { \
                await executeShellCommand({ command: 'timeout-probe', timeout: 30000 }); \
                return [{ name: 'ok' }]; \
            })()",
            input,
            Duration::from_millis(250),
        )
        .await
        .expect("infra");
    assert_eq!(out.suggestions[0].name, "ok");
    let timeouts = runner.timeout_calls();
    assert_eq!(timeouts.len(), 1);
    assert!(
        timeouts[0] <= Duration::from_millis(250),
        "expected timeout to be clamped to JS budget, got {:?}",
        timeouts[0]
    );
}

#[tokio::test]
async fn uncaught_shell_string_denial_is_typed_diagnostic() {
    let worker = JsWorker::spawn().expect("spawn");
    let runner = MockShellRunner::new(Vec::new()).into_arc();
    let mut input = input_with_kind(JsExecutionKind::Custom);
    input.shell_runner = Some(runner);
    input.allow_shell_command = false;

    let out = worker
        .evaluate(
            "(async () => { await executeShellCommand('echo x'); return [{ name: 'no-error' }]; })()",
            input,
            FAST_TIMEOUT,
        )
        .await
        .expect("infra");
    assert!(out.suggestions.is_empty());
    assert_eq!(
        out.diagnostics[0].code,
        JsDiagnosticCode::ShellCommandStringDenied
    );
}

#[tokio::test]
async fn uncaught_shell_call_cap_is_typed_diagnostic() {
    let worker = JsWorker::spawn().expect("spawn");
    let runner = MockShellRunner::new(vec![(
        "echo x",
        Ok(ShellRunOutput {
            stdout: "x\n".into(),
            stderr: String::new(),
            exit_code: Some(0),
        }),
    )])
    .into_arc();
    let mut input = input_with_kind(JsExecutionKind::Custom);
    input.shell_runner = Some(runner);

    let total_calls = MAX_SHELL_CALLS_PER_EVALUATION + 1;
    let program = format!(
        "(async () => {{ for (let i = 0; i < {total_calls}; i++) await executeShellCommand(['echo', 'x']); return [{{ name: 'no-error' }}]; }})()"
    );
    let out = worker
        .evaluate(program, input, FAST_TIMEOUT)
        .await
        .expect("infra");
    assert!(out.suggestions.is_empty());
    assert_eq!(
        out.diagnostics[0].code,
        JsDiagnosticCode::ShellCommandLimitExceeded
    );
}

#[tokio::test]
async fn uncaught_shell_runner_failure_is_typed_diagnostic() {
    let worker = JsWorker::spawn().expect("spawn");
    let runner = MockShellRunner::new(vec![(
        "broken cmd",
        Err(ShellRunError::Spawn("nope".into())),
    )])
    .into_arc();
    let mut input = input_with_kind(JsExecutionKind::Custom);
    input.shell_runner = Some(runner);

    let out = worker
        .evaluate(
            "(async () => { await executeShellCommand(['broken', 'cmd']); return [{ name: 'no-error' }]; })()",
            input,
            FAST_TIMEOUT,
        )
        .await
        .expect("infra");
    assert!(out.suggestions.is_empty());
    assert_eq!(
        out.diagnostics[0].code,
        JsDiagnosticCode::ShellCommandFailed
    );
}

#[tokio::test]
async fn custom_host_api_cwd_env_tokens_visible() {
    let worker = JsWorker::spawn().expect("spawn");
    let mut input = input_with_kind(JsExecutionKind::Custom);
    input.shell_runner = Some(MockShellRunner::new(Vec::new()).into_arc());

    let program = "(async () => [ \
        { name: 'cwd:' + currentWorkingDirectory }, \
        { name: 'home:' + (environmentVariables.HOME || '') }, \
        { name: 'tokens:' + tokens.join(',') }, \
        { name: 'search:' + searchTerm }, \
        { name: 'prev:' + previousToken }, \
    ])()";
    let out = worker
        .evaluate(program, input, FAST_TIMEOUT)
        .await
        .expect("infra");
    let names: Vec<&str> = out.suggestions.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(
        names,
        vec![
            "cwd:/phase5",
            "home:/home/test",
            "tokens:git,checkout",
            "search:ma",
            "prev:checkout",
        ]
    );
}

#[tokio::test]
async fn custom_unsupported_host_api_throws() {
    let worker = JsWorker::spawn().expect("spawn");
    let mut input = input_with_kind(JsExecutionKind::Custom);
    input.shell_runner = Some(MockShellRunner::new(Vec::new()).into_arc());

    let program = "(async () => { \
        try { \
            fig.fs.readFile('/etc/hosts'); \
        } catch (e) { \
            return [{ name: 'caught:' + e.code }]; \
        } \
        return [{ name: 'no-error' }]; \
    })()";
    let out = worker
        .evaluate(program, input, FAST_TIMEOUT)
        .await
        .expect("infra");
    assert_eq!(out.suggestions[0].name, "caught:UnsupportedHostApi");
    // The diagnostic should also surface on the JsRuntimeOutput so the
    // engine's `log_diagnostics` path can pick it up.
    assert!(
        out.diagnostics
            .iter()
            .any(|d| d.code == JsDiagnosticCode::UnsupportedHostApi),
        "expected UnsupportedHostApi diagnostic, got {:?}",
        out.diagnostics,
    );
}

#[tokio::test]
async fn phase5_unsupported_host_namespaces_throw() {
    let worker = JsWorker::spawn().expect("spawn");
    let cases = [
        ("fs.readFile", "fig.fs.readFile('/etc/hosts')"),
        ("path.join", "fig.path.join('a', 'b')"),
        ("keychain.exists", "fig.keychain.exists('id')"),
        ("ipc.readFile", "fig.ipc.readFile('msg')"),
        ("ui.readFile", "fig.ui.readFile('view')"),
    ];
    for (label, call) in cases {
        let mut input = input_with_kind(JsExecutionKind::Custom);
        input.shell_runner = Some(MockShellRunner::new(Vec::new()).into_arc());
        let program = format!(
            "(async () => {{ \
                try {{ \
                    {call}; \
                }} catch (e) {{ \
                    return [{{ name: 'caught:' + e.code }}]; \
                }} \
                return [{{ name: 'no-error' }}]; \
            }})()"
        );
        let out = worker
            .evaluate(program, input, FAST_TIMEOUT)
            .await
            .expect("infra");
        assert_eq!(
            out.suggestions[0].name, "caught:UnsupportedHostApi",
            "{label}: diagnostics={:?}",
            out.diagnostics,
        );
        assert!(
            out.diagnostics
                .iter()
                .any(|d| d.code == JsDiagnosticCode::UnsupportedHostApi),
            "{label}: expected UnsupportedHostApi diagnostic, got {:?}",
            out.diagnostics,
        );
    }
}

#[tokio::test]
async fn custom_shell_command_failure_propagates() {
    let worker = JsWorker::spawn().expect("spawn");
    let runner = MockShellRunner::new(vec![(
        "broken cmd",
        Err(ShellRunError::Spawn("nope".into())),
    )])
    .into_arc();
    let mut input = input_with_kind(JsExecutionKind::Custom);
    input.shell_runner = Some(runner);

    let program = "(async () => { \
        try { \
            await executeShellCommand(['broken', 'cmd']); \
            return [{ name: 'no-error' }]; \
        } catch (e) { \
            return [{ name: 'caught:' + e.code }]; \
        } \
    })()";
    let out = worker
        .evaluate(program, input, FAST_TIMEOUT)
        .await
        .expect("infra");
    assert_eq!(out.suggestions[0].name, "caught:ShellCommandFailed");
}

#[tokio::test]
async fn execute_shell_command_with_no_runner_throws() {
    let worker = JsWorker::spawn().expect("spawn");
    let input = input_with_kind(JsExecutionKind::Custom);
    // No shell_runner installed.
    let program = "(async () => { \
        try { \
            await executeShellCommand(['echo', 'x']); \
            return [{ name: 'no-error' }]; \
        } catch (e) { \
            return [{ name: 'caught:' + e.code }]; \
        } \
    })()";
    let out = worker
        .evaluate(program, input, FAST_TIMEOUT)
        .await
        .expect("infra");
    assert_eq!(out.suggestions[0].name, "caught:ShellCommandFailed");
}

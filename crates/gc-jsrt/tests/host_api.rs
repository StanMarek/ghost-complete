//! Integration tests for the Fig-compatible host API: `script_function`,
//! `custom`, and the synchronous `executeShellCommand` binding.
//!
//! Each test spins up its own [`gc_jsrt::JsWorker`] so a stuck worker
//! cannot bleed into a sibling test. The worker is cheap to spawn —
//! tests still complete in well under a second on a debug build.
//!
//! Contracts under test:
//!
//! - `script_function`: JS evaluates with `(tokens, ctx)` and returns
//!   either an argv array or `{command, args}`. The runtime exposes the
//!   resolved argv via `JsRuntimeOutputPayload::Argv`.
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
    JsDiagnosticCode, JsExecutionKind, JsRuntimeInput, JsRuntimeOutputPayload, JsWorker,
    ShellRunError, ShellRunOutput, ShellRunner, MAX_SHELL_CALLS_PER_EVALUATION,
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
    assert!(
        matches!(
            out.payload,
            JsRuntimeOutputPayload::Argv(ref v)
                if v == &["echo", "git", "ma"]
        ),
        "script_function argv should preserve token order; got payload={:?}, diagnostics={:?}",
        out.payload,
        out.diagnostics
    );
    assert!(
        out.suggestions().is_empty(),
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
    assert!(
        matches!(
            out.payload,
            JsRuntimeOutputPayload::Argv(ref v) if v == &["echo", "hello"]
        ),
        "expected Argv payload, got {:?}",
        out.payload
    );
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
    assert!(out.argv().is_empty());
    assert!(
        matches!(out.payload, JsRuntimeOutputPayload::None),
        "expected None payload, got {:?}",
        out.payload
    );
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
    assert!(out.argv().is_empty());
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
    let names: Vec<&str> = out.suggestions().iter().map(|s| s.name.as_str()).collect();
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
        out.suggestions()[0].name,
        "caught:ShellCommandStringDenied",
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
        out.suggestions()[0].name,
        "ok",
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
    let names: Vec<&str> = out.suggestions().iter().map(|s| s.name.as_str()).collect();
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
        out.suggestions()[0].name,
        "hello:note:7",
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
    assert_eq!(out.suggestions()[0].name, "ok");
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
    assert_eq!(out.suggestions()[0].name, "ok");
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
    assert_eq!(out.suggestions()[0].name, "ok");
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
    assert!(out.suggestions().is_empty());
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
    assert!(out.suggestions().is_empty());
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
    assert!(out.suggestions().is_empty());
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
    let names: Vec<&str> = out.suggestions().iter().map(|s| s.name.as_str()).collect();
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
    assert_eq!(out.suggestions()[0].name, "caught:UnsupportedHostApi");
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
async fn unsupported_host_namespaces_throw() {
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
            out.suggestions()[0].name,
            "caught:UnsupportedHostApi",
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
    assert_eq!(out.suggestions()[0].name, "caught:ShellCommandFailed");
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
    assert_eq!(out.suggestions()[0].name, "caught:ShellCommandFailed");
}

/// Pins precedence: an explicit second `opts` argument wins over a
/// descriptor-embedded `cwd`. The previous behaviour silently inverted
/// this — descriptor cwd would override the more-specific opts cwd.
#[tokio::test]
async fn custom_execute_shell_command_opts_cwd_wins_over_descriptor_cwd() {
    let worker = JsWorker::spawn().expect("spawn");
    let runner = RecordingShellRunner::default().into_arc();
    let mut input = input_with_kind(JsExecutionKind::Custom);
    input.cwd = PathBuf::from("/default");
    input.shell_runner = Some(runner.clone());

    let out = worker
        .evaluate(
            "(async () => { \
                await executeShellCommand( \
                    { command: 'pwd-probe', cwd: '/from-descriptor' }, \
                    { cwd: '/from-opts' } \
                ); \
                return [{ name: 'ok' }]; \
            })()",
            input,
            FAST_TIMEOUT,
        )
        .await
        .expect("infra");
    assert_eq!(out.suggestions()[0].name, "ok");
    assert_eq!(
        runner.cwd_calls(),
        vec![PathBuf::from("/from-opts")],
        "explicit opts.cwd should win over descriptor.cwd",
    );
}

/// Pins the operator-visible diagnostic when `opts.cwd` is the wrong
/// type. The diagnostic string is part of the doctor surface — a future
/// refactor that silently swaps the typed branch for `as_string().unwrap_or_default()`
/// would erase the spec-author hint without this test catching it.
#[tokio::test]
async fn custom_execute_shell_command_bad_typed_cwd_emits_diagnostic() {
    let worker = JsWorker::spawn().expect("spawn");
    let runner = RecordingShellRunner::default().into_arc();
    let mut input = input_with_kind(JsExecutionKind::Custom);
    input.shell_runner = Some(runner);

    let out = worker
        .evaluate(
            "(async () => { \
                await executeShellCommand(['echo', 'x'], { cwd: 42 }); \
                return [{ name: 'ok' }]; \
            })()",
            input,
            FAST_TIMEOUT,
        )
        .await
        .expect("infra");
    assert_eq!(out.suggestions()[0].name, "ok");
    assert!(
        out.diagnostics
            .iter()
            .any(|d| d.code == JsDiagnosticCode::UnsupportedHostApi
                && d.message == "executeShellCommand.options.cwd<bad-type>"),
        "expected cwd<bad-type> diagnostic, got {:?}",
        out.diagnostics,
    );
}

/// Same as the cwd case but for timeouts.
#[tokio::test]
async fn custom_execute_shell_command_bad_typed_timeout_emits_diagnostic() {
    let worker = JsWorker::spawn().expect("spawn");
    let runner = RecordingShellRunner::default().into_arc();
    let mut input = input_with_kind(JsExecutionKind::Custom);
    input.shell_runner = Some(runner);

    let out = worker
        .evaluate(
            "(async () => { \
                await executeShellCommand(['echo', 'x'], { timeout: 'fast' }); \
                return [{ name: 'ok' }]; \
            })()",
            input,
            FAST_TIMEOUT,
        )
        .await
        .expect("infra");
    assert_eq!(out.suggestions()[0].name, "ok");
    assert!(
        out.diagnostics
            .iter()
            .any(|d| d.code == JsDiagnosticCode::UnsupportedHostApi
                && d.message == "executeShellCommand.options.timeout<bad-type>"),
        "expected timeout<bad-type> diagnostic, got {:?}",
        out.diagnostics,
    );
}

/// Pins the diagnostic for a non-finite or out-of-range timeout
/// (NaN / Infinity / negative). Without this, `f64 as u64`'s saturating
/// cast would silently turn `timeout: -1` into 0ms.
#[tokio::test]
async fn custom_execute_shell_command_out_of_range_timeout_emits_diagnostic() {
    let worker = JsWorker::spawn().expect("spawn");
    let runner = RecordingShellRunner::default().into_arc();
    let mut input = input_with_kind(JsExecutionKind::Custom);
    input.shell_runner = Some(runner);

    let out = worker
        .evaluate(
            "(async () => { \
                await executeShellCommand(['echo', 'x'], { timeout: -1 }); \
                return [{ name: 'ok' }]; \
            })()",
            input,
            FAST_TIMEOUT,
        )
        .await
        .expect("infra");
    assert_eq!(out.suggestions()[0].name, "ok");
    assert!(
        out.diagnostics
            .iter()
            .any(|d| d.code == JsDiagnosticCode::UnsupportedHostApi
                && d.message == "executeShellCommand.options.timeout<out-of-range>"),
        "expected timeout<out-of-range> diagnostic, got {:?}",
        out.diagnostics,
    );
}

/// Pins the diagnostic when the WHOLE opts argument is non-object — the
/// typical mistake is a positional timeout: `executeShellCommand([...], 5000)`.
/// Without this, the silent fallback would mask the misuse.
#[tokio::test]
async fn custom_execute_shell_command_non_object_opts_emits_diagnostic() {
    let worker = JsWorker::spawn().expect("spawn");
    let runner = RecordingShellRunner::default().into_arc();
    let mut input = input_with_kind(JsExecutionKind::Custom);
    input.shell_runner = Some(runner);

    let out = worker
        .evaluate(
            "(async () => { \
                await executeShellCommand(['echo', 'x'], 5000); \
                return [{ name: 'ok' }]; \
            })()",
            input,
            FAST_TIMEOUT,
        )
        .await
        .expect("infra");
    assert_eq!(out.suggestions()[0].name, "ok");
    assert!(
        out.diagnostics
            .iter()
            .any(|d| d.code == JsDiagnosticCode::UnsupportedHostApi
                && d.message == "executeShellCommand.options<not-object>"),
        "expected options<not-object> diagnostic, got {:?}",
        out.diagnostics,
    );
}

/// Pins the `executeShellCommand.options.cwd<decode-failure>` branch:
/// QuickJS exposes JS strings as UTF-16 internally and `to_string()`
/// fails when asked to materialise a lone unpaired surrogate as UTF-8.
/// The host code records an `UnsupportedHostApi` diagnostic so the
/// spec author hunting "why is my custom cwd silently ignored" gets a
/// signal rather than a silent fallback to the input cwd.
///
/// Without this pin, a future refactor that swaps `s.to_string().ok()?`
/// for an infallible accessor (or that silently drops the diagnostic
/// line) would erase the typed signal without test failure.
#[tokio::test]
async fn custom_execute_shell_command_cwd_decode_failure_emits_diagnostic() {
    let worker = JsWorker::spawn().expect("spawn");
    let runner = RecordingShellRunner::default().into_arc();
    let mut input = input_with_kind(JsExecutionKind::Custom);
    input.shell_runner = Some(runner);

    // `String.fromCharCode(0xD800)` constructs a single UTF-16 code unit
    // that is a lone surrogate — invalid by itself in UTF-8. QuickJS'
    // `to_string()` rejects the conversion, which trips the typed
    // diagnostic in the host parser.
    let out = worker
        .evaluate(
            "(async () => { \
                const badCwd = String.fromCharCode(0xD800); \
                await executeShellCommand(['echo', 'x'], { cwd: badCwd }); \
                return [{ name: 'ok' }]; \
            })()",
            input,
            FAST_TIMEOUT,
        )
        .await
        .expect("infra");
    assert_eq!(out.suggestions()[0].name, "ok");
    assert!(
        out.diagnostics
            .iter()
            .any(|d| d.code == JsDiagnosticCode::UnsupportedHostApi
                && d.message == "executeShellCommand.options.cwd<decode-failure>"),
        "expected cwd<decode-failure> diagnostic, got {:?}",
        out.diagnostics,
    );
}

/// Pins the SHELL_TIMEOUT_FLOOR short-circuit: when the JS deadline has
/// fewer than 5ms remaining, `bounded_shell_timeout` returns None and
/// the runner is bypassed entirely. A regression that lowers the floor
/// to 0 would silently re-introduce the 5–15ms macOS fork+reap waste
/// cycle this guards against.
#[tokio::test]
async fn custom_execute_shell_command_below_floor_skips_spawn_and_returns_timeout() {
    let worker = JsWorker::spawn().expect("spawn");
    let runner = RecordingShellRunner::default().into_arc();
    let mut input = input_with_kind(JsExecutionKind::Custom);
    input.shell_runner = Some(runner.clone());

    // Burn nearly the entire JS budget before issuing the shell call so
    // the call sees < 5ms remaining and short-circuits without spawning.
    // The JS busy-loop polls Date.now() so it's bounded by wall-clock
    // rather than instruction count.
    let out = worker
        .evaluate(
            "(async () => { \
                const start = Date.now(); \
                while (Date.now() - start < 95) { /* burn budget */ } \
                try { \
                    await executeShellCommand(['echo', 'x']); \
                    return [{ name: 'no-error' }]; \
                } catch (e) { \
                    return [{ name: 'caught:' + e.code }]; \
                } \
            })()",
            input,
            Duration::from_millis(100),
        )
        .await
        .expect("infra");
    // Either the JS deadline tripped before the shell call (Timeout
    // diagnostic, no suggestion) OR the shell call short-circuited
    // (caught a ShellCommandFailed wrapping the timeout). In both cases
    // the runner must NOT have spawned anything.
    assert!(
        runner.timeout_calls().is_empty(),
        "no subprocess should have been spawned when the JS deadline is exhausted; got {} calls",
        runner.timeout_calls().len(),
    );
    if !out.suggestions().is_empty() {
        assert_eq!(
            out.suggestions()[0].name,
            "caught:ShellCommandFailed",
            "if the shell call returned, it should have surfaced a ShellCommandFailed (timeout); diagnostics={:?}",
            out.diagnostics,
        );
    }
}

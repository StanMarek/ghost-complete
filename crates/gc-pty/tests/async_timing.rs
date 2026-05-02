//! Integration tests for the bounded-block render + merge-on-arrival behaviour.
//!
//! These tests exercise `prepare_trigger_with_block` + `apply_block_result`
//! directly, injecting a controlled channel receiver so timing can be
//! asserted without depending on real generator speed or flakiness.
//!
//! The harness pattern mirrors the unit tests in `handler.rs` (see
//! `test_try_merge_dynamic_*`): build an `InputHandler`, prime its
//! `dynamic_rx` / `dynamic_task` fields via the public API, then drive
//! the async select from the test.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gc_pty::dynamic_result::{DynamicResult, ProviderTag};
use gc_pty::handler::InputHandler;
use gc_suggest::types::{Suggestion, SuggestionKind, SuggestionSource};
use tokio::sync::mpsc;

fn loaded(items: Vec<Suggestion>) -> DynamicResult {
    DynamicResult::Loaded {
        provider: ProviderTag::Git(gc_suggest::git::GitQueryKind::Branches),
        suggestions: items,
    }
}

fn empty_result() -> DynamicResult {
    DynamicResult::Empty {
        provider: ProviderTag::Git(gc_suggest::git::GitQueryKind::Branches),
    }
}

// NOTE: gc_terminal::TerminalProfile::for_ghostty() is only available in
// dev/test builds via the "test-utils" feature (declared in gc-pty's
// dev-dependencies). We construct the handler via the public constructor
// instead and avoid importing the test-only type directly.

/// Build a minimal handler suitable for timing tests.
/// `render_block_ms` is set from the argument; spec dirs point to "." so
/// no real specs load (the handler init is fast and always succeeds).
fn make_test_handler(render_block_ms: u64) -> InputHandler {
    InputHandler::new(
        &[PathBuf::from(".")],
        gc_terminal::TerminalProfile::for_ghostty(),
    )
    .expect("handler init failed")
    .with_render_block_ms(render_block_ms)
}

/// Build a visible handler with `render_block_ms` configured and a
/// pre-seeded `dynamic_rx` that delivers `async_items` after `delay`.
///
/// Returns `(handler, sender_task)`. The handler owns the dynamic_rx
/// channel (primed via `restore_dynamic_rx`); the `sender_task` is awaited
/// in the test to ensure clean shutdown.
fn make_handler_with_delayed_rx(
    render_block_ms: u64,
    async_items: Vec<Suggestion>,
    delay: Duration,
) -> (InputHandler, tokio::task::JoinHandle<()>) {
    let mut handler = make_test_handler(render_block_ms);

    // Prime the handler as if spawn_generators already ran.
    // buffer_generation was incremented in prepare_trigger_with_block, but
    // for these tests we set spawned_generation manually to match.
    handler.set_buffer_generation(1);
    handler.set_spawned_generation(1);
    // Prime dynamic_ctx so the staleness check in try_merge_dynamic passes.
    handler.prime_dynamic_ctx_for_empty_buffer();

    let (tx, rx) = mpsc::channel::<DynamicResult>(1);
    handler.restore_dynamic_rx(rx);

    // Spawn a task that sends async_items after `delay`.
    let sender = tokio::spawn(async move {
        tokio::time::sleep(delay).await;
        let _ = tx.send(loaded(async_items)).await;
    });

    (handler, sender)
}

/// Construct a dummy parser (no real shell). The tests never call `trigger()`
/// directly, so we just need a valid parser Arc.
fn make_parser() -> Arc<Mutex<gc_parser::TerminalParser>> {
    Arc::new(Mutex::new(gc_parser::TerminalParser::new(24, 80)))
}

/// Build a single sync suggestion of kind `Flag` (base priority 30) so that
/// a GitBranch generator (base priority 80) will trigger the blocking path.
fn flag_suggestion(text: &str) -> Suggestion {
    Suggestion {
        text: text.to_string(),
        kind: SuggestionKind::Flag,
        source: SuggestionSource::Spec,
        ..Default::default()
    }
}

fn branch_suggestion(text: &str) -> Suggestion {
    Suggestion {
        text: text.to_string(),
        kind: SuggestionKind::GitBranch,
        source: SuggestionSource::Git,
        ..Default::default()
    }
}

// ── Test 1 ───────────────────────────────────────────────────────────────────

/// Generator returns within 30 ms; render_block_ms = 80 ms.
/// Expected: `apply_block_result` receives the async items (Some(…)) and
/// `maybe_async` is not None — i.e. the generator completed before the timeout.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fast_async_arrives_before_block_window() {
    let sync_suggestions = vec![flag_suggestion("--verbose")];
    let async_items = vec![branch_suggestion("main"), branch_suggestion("dev")];

    let (mut handler, sender_task) = make_handler_with_delayed_rx(
        80, // block_ms
        async_items.clone(),
        Duration::from_millis(30), // generator "completes" in 30 ms
    );

    // Simulate the blocked state: take the rx back out as prepare_trigger_with_block would.
    let rx = handler
        .take_dynamic_rx()
        .expect("rx should be set by make_handler_with_delayed_rx");

    let start = Instant::now();
    let timeout_dur = Duration::from_millis(80);

    // Mirror what debounce_loop does in Phase 2.
    let (maybe_async, rx_on_timeout): (Option<DynamicResult>, Option<mpsc::Receiver<_>>) = {
        let mut rx = rx;
        tokio::select! {
            maybe_result = rx.recv() => (maybe_result, None),
            _ = tokio::time::sleep(timeout_dur) => (None, Some(rx)),
        }
    };

    let elapsed = start.elapsed();

    // The generator completed at ~30 ms, so we expect the result before timeout.
    assert!(
        maybe_async.is_some(),
        "generator should have completed before 80 ms timeout, elapsed: {:?}",
        elapsed
    );
    assert!(
        rx_on_timeout.is_none(),
        "timeout path should not have fired when generator was fast"
    );

    let async_results = maybe_async.unwrap();
    match &async_results {
        DynamicResult::Loaded { suggestions, .. } => {
            assert_eq!(suggestions.len(), 2, "expected 2 branch suggestions")
        }
        other => panic!("expected loaded result, got {other:?}"),
    }

    // Now call apply_block_result.
    let parser = make_parser();
    let mut render_buf = Vec::new();
    handler.apply_block_result(
        &parser,
        &mut render_buf,
        Some(async_results),
        None,
        None,
        sync_suggestions.clone(),
        0,
        0,
        24,
        80,
        (0, 0), // fingerprint
        "",     // current_word: empty in this fixture
    );

    // After merge: suggestions should contain both sync and async items.
    assert!(
        handler.current_suggestions().len() >= 2,
        "merged suggestions should include async branches, got: {:?}",
        handler
            .current_suggestions()
            .iter()
            .map(|s| &s.text)
            .collect::<Vec<_>>()
    );
    assert!(
        handler
            .current_suggestions()
            .iter()
            .any(|s| s.kind == SuggestionKind::GitBranch),
        "GitBranch suggestions should be present after merge"
    );

    // The elapsed time should be around 30 ms, well under 80 ms.
    assert!(
        elapsed < Duration::from_millis(75),
        "fast generator should complete well before timeout, elapsed: {:?}",
        elapsed
    );

    sender_task.await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn block_result_keeps_receiver_when_more_batches_may_arrive() {
    let mut handler = make_test_handler(80);
    handler.set_buffer_generation(1);
    handler.set_spawned_generation(1);
    handler.prime_dynamic_ctx_for_empty_buffer();

    let (tx, mut rx) = mpsc::channel::<DynamicResult>(2);
    tx.send(loaded(vec![branch_suggestion("main")]))
        .await
        .unwrap();
    let first = rx.recv().await.expect("first dynamic batch");

    let parser = make_parser();
    let mut render_buf = Vec::new();
    handler.apply_block_result(
        &parser,
        &mut render_buf,
        Some(first),
        Some(rx),
        None,
        vec![flag_suggestion("--verbose")],
        0,
        0,
        24,
        80,
        (0, 0),
        "",
    );

    assert!(
        handler.has_dynamic_rx(),
        "open receiver must be restored after a partial bounded-block batch"
    );
    assert!(
        handler
            .current_suggestions()
            .iter()
            .any(|s| s.text == "main"),
        "first batch should still merge immediately"
    );

    tx.send(loaded(vec![branch_suggestion("dev")]))
        .await
        .unwrap();
    drop(tx);
    let mut render_buf = Vec::new();
    assert!(handler.try_merge_dynamic(&parser, &mut render_buf));

    let texts: Vec<&str> = handler
        .current_suggestions()
        .iter()
        .map(|s| s.text.as_str())
        .collect();
    assert!(
        texts.contains(&"main") && texts.contains(&"dev"),
        "later dynamic batches must not be lost: {texts:?}"
    );
    assert!(
        !handler.has_dynamic_rx(),
        "receiver clears once the final batch disconnects"
    );
}

// ── Test 2 ───────────────────────────────────────────────────────────────────

/// Generator returns at 200 ms; render_block_ms = 80 ms.
/// Expected: timeout fires at ~80 ms (first paint shows sync only);
/// the rx is returned in `rx_on_timeout` for dynamic_merge_loop to use.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn slow_async_falls_through_then_merges_on_arrival() {
    let sync_suggestions = vec![flag_suggestion("--verbose")];
    let async_items = vec![branch_suggestion("main")];

    let (mut handler, sender_task) =
        make_handler_with_delayed_rx(80, async_items.clone(), Duration::from_millis(200));

    let rx = handler
        .take_dynamic_rx()
        .expect("rx should be set by make_handler_with_delayed_rx");

    let start = Instant::now();
    let timeout_dur = Duration::from_millis(80);

    let (maybe_async, rx_on_timeout): (Option<DynamicResult>, Option<mpsc::Receiver<_>>) = {
        let mut rx = rx;
        tokio::select! {
            maybe_result = rx.recv() => (maybe_result, None),
            _ = tokio::time::sleep(timeout_dur) => (None, Some(rx)),
        }
    };

    let elapsed_phase1 = start.elapsed();

    // Timeout should have fired at ~80 ms.
    assert!(
        maybe_async.is_none(),
        "generator should NOT have completed within 80 ms, elapsed: {:?}",
        elapsed_phase1
    );
    assert!(
        rx_on_timeout.is_some(),
        "rx_on_timeout should be returned on timeout path"
    );
    assert!(
        elapsed_phase1 >= Duration::from_millis(70),
        "timeout should take at least 70 ms, elapsed: {:?}",
        elapsed_phase1
    );

    // Phase 1 result: apply_block_result with no async, but restore rx.
    let rx_restored = rx_on_timeout;
    let parser = make_parser();
    let mut render_buf = Vec::new();
    handler.apply_block_result(
        &parser,
        &mut render_buf,
        None, // timeout: no async results
        None,
        rx_restored,
        sync_suggestions,
        0,
        0,
        24,
        80,
        (0, 0),
        "", // current_word: empty in this fixture
    );

    // After timeout apply: rx should be restored in handler for dynamic_merge_loop.
    assert!(
        handler.has_dynamic_rx(),
        "dynamic_rx should be restored after timeout so dynamic_merge_loop can use it"
    );

    // Simulate dynamic_merge_loop picking up the result at ~200 ms.
    // Wait for the sender task to complete.
    sender_task.await.unwrap();

    // Now try_merge_dynamic should have a result available.
    let mut render_buf2 = Vec::new();
    let merged = handler.try_merge_dynamic(&parser, &mut render_buf2);

    let elapsed_total = start.elapsed();
    assert!(
        merged,
        "try_merge_dynamic should have merged async results after generator completed"
    );
    assert!(
        elapsed_total >= Duration::from_millis(180),
        "total elapsed should be at least 180 ms (generator delay), elapsed: {:?}",
        elapsed_total
    );
    assert!(
        handler
            .current_suggestions()
            .iter()
            .any(|s| s.kind == SuggestionKind::GitBranch),
        "GitBranch suggestions should appear after merge-on-arrival"
    );
}

// ── Test 3 ───────────────────────────────────────────────────────────────────

/// Generator returns at 200 ms; render_block_ms = 80 ms; keystroke arrives
/// at ~30 ms cancelling the block window.
/// Expected: the select exits early (before timeout and before generator),
/// rx is restored in the handler, and no merged paint happens for the old buffer.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn keystroke_during_wait_cancels_block() {
    let sync_suggestions = vec![flag_suggestion("--verbose")];
    let async_items = vec![branch_suggestion("main")];

    let (mut handler, sender_task) =
        make_handler_with_delayed_rx(80, async_items, Duration::from_millis(200));

    let rx = handler.take_dynamic_rx().expect("rx should be set");

    // Simulate the "keystroke notify" by firing after 30 ms.
    let keystroke_notify = Arc::new(tokio::sync::Notify::new());
    let kn_clone = Arc::clone(&keystroke_notify);
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(30)).await;
        kn_clone.notify_one();
    });

    // Seed dynamic_task with a noop handle so we can assert the production
    // cancel code aborts and clears it.
    handler.seed_dynamic_task_noop();
    assert!(
        handler.has_dynamic_task(),
        "setup: dynamic_task must be seeded before exercising the cancel path"
    );

    let start = Instant::now();
    let timeout_dur = Duration::from_millis(80);

    // Three-way select mirrors the debounce_loop Phase 2 in proxy.rs.
    // Only the keystroke-cancellation arm is the happy path here; the other
    // arms exist as panic guards.
    #[derive(Debug)]
    enum Outcome {
        AsyncCompleted,
        Timeout,
        KeystrokeCancelled,
    }

    let outcome = {
        let mut rx = rx;
        tokio::select! {
            _maybe_result = rx.recv() => Outcome::AsyncCompleted,
            _ = tokio::time::sleep(timeout_dur) => Outcome::Timeout,
            _ = keystroke_notify.notified() => {
                drop(rx);
                Outcome::KeystrokeCancelled
            }
        }
    };

    let elapsed = start.elapsed();

    match outcome {
        Outcome::KeystrokeCancelled => {
            // The block was cancelled at ~30 ms.
            assert!(
                elapsed < Duration::from_millis(70),
                "keystroke cancellation should fire around 30 ms, elapsed: {:?}",
                elapsed
            );
            // Mirror the production cancel arm in proxy.rs: abort the orphan
            // generator task and clear dynamic_ctx so the next trigger starts
            // clean. (Production also calls notify.notify_one() to re-arm the
            // outer debounce loop; we assert state, not scheduling, here.)
            handler.abort_dynamic_task_and_clear_ctx();
            assert!(
                !handler.has_dynamic_task(),
                "abort_dynamic_task_and_clear_ctx must clear dynamic_task so the orphan generator's results don't land in a None rx"
            );
            // No apply_block_result was called — no paint for the old buffer.
            // The suggestions remain at sync-only state (no branches merged).
            assert!(
                handler
                    .current_suggestions()
                    .iter()
                    .all(|s| s.kind != SuggestionKind::GitBranch),
                "no GitBranch suggestions should have been merged after keystroke cancellation"
            );
        }
        Outcome::Timeout => {
            panic!("expected keystroke cancellation at ~30 ms, but timeout fired at ~80 ms");
        }
        Outcome::AsyncCompleted => {
            panic!(
                "expected keystroke cancellation, but async generator completed (it should take 200 ms)"
            );
        }
    }

    // Cleanup.
    sender_task.abort();
    let _ = sender_task.await;

    drop(sync_suggestions);
}

// ── Test 4 ───────────────────────────────────────────────────────────────────

/// Generator returns within the block window AND the user has typed a
/// non-empty current_word. `apply_block_result` must rank the merged pool
/// against the live query so candidates that don't match get dropped.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fast_async_with_typed_query_refilters_pool() {
    let sync_suggestions = vec![flag_suggestion("--main"), flag_suggestion("--zzz")];
    let async_items = vec![branch_suggestion("main"), branch_suggestion("feature-x")];

    let mut handler = make_test_handler(80);
    handler.set_buffer_generation(1);
    handler.set_spawned_generation(1);

    // Drive the parser to a buffer whose live current_word is "main" so
    // `apply_block_result`'s staleness check sees a fresh, matching query.
    let parser = Arc::new(Mutex::new(gc_parser::TerminalParser::new(24, 80)));
    {
        let mut p = parser.lock().unwrap();
        p.state_mut()
            .predict_command_buffer("git checkout main".to_string(), 17);
    }
    handler.prime_dynamic_ctx_for_buffer("git checkout main", 17);

    let mut render_buf = Vec::new();
    handler.apply_block_result(
        &parser,
        &mut render_buf,
        Some(loaded(async_items)),
        None,
        None,
        sync_suggestions,
        0,
        0,
        24,
        80,
        (0, 0),
        "main",
    );

    let texts: Vec<String> = handler
        .current_suggestions()
        .iter()
        .map(|s| s.text.clone())
        .collect();
    assert!(
        texts.contains(&"main".to_string()),
        "branch 'main' should pass the 'main' query, got: {texts:?}"
    );
    assert!(
        texts.contains(&"--main".to_string()),
        "flag '--main' should pass the 'main' query, got: {texts:?}"
    );
    assert!(
        !texts.contains(&"--zzz".to_string()),
        "flag '--zzz' must be filtered out by the 'main' query, got: {texts:?}"
    );
    assert!(
        !texts.contains(&"feature-x".to_string()),
        "branch 'feature-x' must be filtered out by the 'main' query, got: {texts:?}"
    );
}

// ── Test 5 ───────────────────────────────────────────────────────────────────

/// Generator returned `Some(vec![])` within the block window (the empty-pool
/// case the `Some([])` arm has to handle). `apply_block_result` must clear
/// `dynamic_task` and `dynamic_rx`, and stamp the trigger fingerprint so the
/// idempotency guard short-circuits the next identical trigger.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn empty_async_result_clears_loading_state() {
    let mut handler = make_test_handler(80);
    handler.set_buffer_generation(1);
    handler.set_spawned_generation(1);
    handler.prime_dynamic_ctx_for_empty_buffer();
    handler.seed_dynamic_task_noop();
    assert!(
        handler.has_dynamic_task(),
        "setup: dynamic_task must be seeded before apply_block_result"
    );

    let parser = make_parser();
    let mut render_buf = Vec::new();
    let fp = (0xdead_beef_u64, 7_usize);

    handler.apply_block_result(
        &parser,
        &mut render_buf,
        Some(empty_result()),
        None,
        None,
        Vec::new(),
        0,
        0,
        24,
        80,
        fp,
        "",
    );

    assert!(
        !handler.has_dynamic_rx(),
        "empty async result must clear dynamic_rx"
    );
    assert!(
        !handler.has_dynamic_task(),
        "empty async result must clear dynamic_task"
    );
    assert_eq!(
        handler.last_trigger_fingerprint(),
        Some(fp),
        "empty async result must still stamp the trigger fingerprint"
    );
}

// ── Tests 6 & 7 ──────────────────────────────────────────────────────────────
//
// `prepare_trigger_with_block` should return `NeedsBlock` when sync results
// only contain low-priority candidates AND a high-priority generator (e.g.
// git_branches) is pending — but only when `render_block_ms > 0`. With
// render_block_ms == 0 the bounded-block window is disabled and the function
// must return `Painted` immediately so dynamic_merge_loop handles the result.

/// Write a minimal git spec into a temporary directory so the engine resolves
/// `git checkout <TAB>` to a SyncResult containing `git_generators=[Branches]`.
fn write_test_git_spec(dir: &std::path::Path) {
    use std::io::Write;
    let path = dir.join("git.json");
    let spec = r#"{
        "name": "git",
        "subcommands": [
            {
                "name": "checkout",
                "options": [
                    { "name": ["-q", "--quiet"], "description": "Quiet" }
                ],
                "args": [
                    {
                        "name": "branch",
                        "generators": [{ "type": "git_branches" }]
                    }
                ]
            }
        ]
    }"#;
    let mut f = std::fs::File::create(&path).expect("create test git spec");
    f.write_all(spec.as_bytes()).expect("write test git spec");
}

fn make_handler_with_git_spec(render_block_ms: u64) -> (InputHandler, tempfile::TempDir) {
    let tmp = tempfile::tempdir().expect("tempdir");
    write_test_git_spec(tmp.path());
    let handler = InputHandler::new(
        &[tmp.path().to_path_buf()],
        gc_terminal::TerminalProfile::for_ghostty(),
    )
    .expect("handler init failed")
    .with_render_block_ms(render_block_ms);
    (handler, tmp)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn prepare_trigger_returns_needs_block_when_branch_generator_pending() {
    let (mut handler, _tmp) = make_handler_with_git_spec(80);

    let parser = Arc::new(Mutex::new(gc_parser::TerminalParser::new(24, 80)));
    {
        let mut p = parser.lock().unwrap();
        p.state_mut()
            .predict_command_buffer("git checkout ".to_string(), 13);
    }

    let mut render_buf = Vec::new();
    let prepared = handler.prepare_trigger_with_block(&parser, &mut render_buf);

    match prepared {
        gc_pty::handler::TriggerPrepared::NeedsBlock { block_ms, .. } => {
            assert_eq!(block_ms, 80, "block_ms must echo render_block_ms");
        }
        gc_pty::handler::TriggerPrepared::Painted => {
            panic!("expected NeedsBlock for git checkout with branches generator, got Painted")
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn prepare_trigger_returns_painted_when_block_ms_zero() {
    let (mut handler, _tmp) = make_handler_with_git_spec(0);

    let parser = Arc::new(Mutex::new(gc_parser::TerminalParser::new(24, 80)));
    {
        let mut p = parser.lock().unwrap();
        p.state_mut()
            .predict_command_buffer("git checkout ".to_string(), 13);
    }

    let mut render_buf = Vec::new();
    let prepared = handler.prepare_trigger_with_block(&parser, &mut render_buf);

    assert!(
        matches!(prepared, gc_pty::handler::TriggerPrepared::Painted),
        "render_block_ms=0 must short-circuit to Painted regardless of pending generators"
    );
}

// ── Test 8 ───────────────────────────────────────────────────────────────────

/// `apply_block_result` must drop async results when the live buffer no
/// longer matches the spawn-time context. The Stale arm of `MergeFreshness`
/// returns without merging and without stamping the trigger fingerprint.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stale_async_result_dropped_when_buffer_drifted() {
    let mut handler = make_test_handler(80);
    handler.set_buffer_generation(1);
    handler.set_spawned_generation(1);
    // Pin the spawn-time context to `git checkout main`.
    handler.prime_dynamic_ctx_for_buffer("git checkout main", 17);

    // Drive the parser to a different command. The staleness check compares
    // command/args/preceding_flag/word_index — `git status` differs from
    // `git checkout main` on args, so the result must be dropped.
    let parser = Arc::new(Mutex::new(gc_parser::TerminalParser::new(24, 80)));
    {
        let mut p = parser.lock().unwrap();
        p.state_mut()
            .predict_command_buffer("git status".to_string(), 10);
    }

    let mut render_buf = Vec::new();
    handler.apply_block_result(
        &parser,
        &mut render_buf,
        Some(loaded(vec![branch_suggestion("main")])),
        None,
        None,
        Vec::new(),
        0,
        0,
        24,
        80,
        (0xfeed_face_u64, 5),
        "main",
    );

    assert!(
        !handler
            .current_suggestions()
            .iter()
            .any(|s| s.text == "main"),
        "stale buffer drift must drop async branches"
    );
    assert!(
        handler.last_trigger_fingerprint().is_none(),
        "Stale arm must NOT stamp the trigger fingerprint"
    );
}

// ── Test 9 ───────────────────────────────────────────────────────────────────

/// `apply_block_result` must drop async results when `spawned_generation`
/// no longer matches `buffer_generation`, even if the spawn-time context
/// would otherwise still match the live buffer. Proves the generation
/// guard fires before the ctx-equality check.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stale_generation_drops_async_result() {
    let mut handler = make_test_handler(80);
    handler.set_buffer_generation(5);
    handler.set_spawned_generation(2);
    // Prime ctx for the SAME buffer the parser holds so the ctx-equality
    // check would otherwise pass — only the generation mismatch should
    // trigger the drop.
    handler.prime_dynamic_ctx_for_buffer("git checkout main", 17);

    let parser = Arc::new(Mutex::new(gc_parser::TerminalParser::new(24, 80)));
    {
        let mut p = parser.lock().unwrap();
        p.state_mut()
            .predict_command_buffer("git checkout main".to_string(), 17);
    }

    let mut render_buf = Vec::new();
    handler.apply_block_result(
        &parser,
        &mut render_buf,
        Some(loaded(vec![branch_suggestion("main")])),
        None,
        None,
        Vec::new(),
        0,
        0,
        24,
        80,
        (0, 0),
        "main",
    );

    assert!(
        !handler
            .current_suggestions()
            .iter()
            .any(|s| s.text == "main"),
        "generation mismatch must drop async branches before the ctx-equality check"
    );
}

// ── Test 10 ──────────────────────────────────────────────────────────────────

/// The keystroke-cancel arm of the bounded-block `select!` in `debounce_loop`
/// calls `notify.notify_one()` after consuming a `notified()` permit so the
/// outer debounce loop fires immediately instead of waiting on the next
/// keystroke. This test mirrors that pattern in isolation: consume the
/// permit, re-arm via `notify_one`, then `select!` the second `notified()`
/// against a 50ms timeout — the second `notified()` must resolve.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn keystroke_cancel_rearms_notify_for_outer_loop() {
    use tokio::sync::Notify;

    let notify = Arc::new(Notify::new());
    let n_clone = Arc::clone(&notify);

    // Background task: consume the first permit, then re-arm. Mirrors the
    // keystroke-cancel arm of the bounded-block `select!` in `debounce_loop`.
    let task = tokio::spawn(async move {
        // Initial notify so the first notified() resolves.
        n_clone.notify_one();
        n_clone.notified().await;
        n_clone.notify_one();
    });

    // Wait for the task to consume + re-arm before we race notified() vs timeout.
    task.await.unwrap();

    let timed_out = tokio::select! {
        _ = notify.notified() => false,
        _ = tokio::time::sleep(Duration::from_millis(50)) => true,
    };

    assert!(
        !timed_out,
        "the re-arm via notify_one() must let a follow-up notified() resolve before the 50ms timeout"
    );
}

// ── Test 11 ──────────────────────────────────────────────────────────────────

/// Write a minimal spec whose positional arg has ONLY a script generator —
/// no flags on the subcommand, no template, no static suggestions. Resolving
/// `mycmd <space>` produces an empty `sync_suggestions` plus a pending
/// `script_generators` entry.
fn write_test_script_spec(dir: &std::path::Path) {
    use std::io::Write;
    let path = dir.join("mycmd.json");
    let spec = r#"{
        "name": "mycmd",
        "args": [
            {
                "name": "value",
                "generators": [
                    {
                        "script": ["sleep", "10"],
                        "transforms": ["split_lines", "filter_empty"]
                    }
                ]
            }
        ]
    }"#;
    let mut f = std::fs::File::create(&path).expect("create test script spec");
    f.write_all(spec.as_bytes())
        .expect("write test script spec");
}

/// Reorder regression: `prepare_trigger_with_block` must `dismiss()` BEFORE
/// `spawn_generators()`. If dismiss ran after, it would abort the freshly-
/// spawned task and drop the rx the generator just opened.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dismiss_then_spawn_keeps_dynamic_rx_alive_when_visible_with_empty_sync() {
    let tmp = tempfile::tempdir().expect("tempdir");
    write_test_script_spec(tmp.path());
    let mut handler = InputHandler::new(
        &[tmp.path().to_path_buf()],
        gc_terminal::TerminalProfile::for_ghostty(),
    )
    .expect("handler init failed")
    .with_render_block_ms(0);

    // Mark the popup visible as if a previous trigger had populated it.
    handler.set_visible(true);

    let parser = Arc::new(Mutex::new(gc_parser::TerminalParser::new(24, 80)));
    {
        let mut p = parser.lock().unwrap();
        p.state_mut()
            .predict_command_buffer("mycmd ".to_string(), 6);
    }

    let mut render_buf = Vec::new();
    let prepared = handler.prepare_trigger_with_block(&parser, &mut render_buf);

    assert!(
        matches!(prepared, gc_pty::handler::TriggerPrepared::Painted),
        "render_block_ms=0 must short-circuit to Painted regardless of pending generators"
    );
    assert!(
        handler.has_dynamic_rx(),
        "spawn must outlive the dismiss — dynamic_rx should be Some after the reordered call"
    );
    assert!(
        handler.has_dynamic_task(),
        "spawn must outlive the dismiss — dynamic_task should be Some after the reordered call"
    );
}

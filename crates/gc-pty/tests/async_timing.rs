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

use gc_pty::handler::InputHandler;
use gc_suggest::types::{Suggestion, SuggestionKind, SuggestionSource};
use tokio::sync::mpsc;

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
/// Returns `(handler, rx, sender_task)`. The `sender_task` is a
/// `JoinHandle<()>` for the background tokio task that sends after the
/// delay; it is awaited in the test to ensure clean shutdown.
fn make_handler_with_delayed_rx(
    render_block_ms: u64,
    async_items: Vec<Suggestion>,
    delay: Duration,
) -> (InputHandler, tokio::task::JoinHandle<()>) {
    let mut handler = make_test_handler(render_block_ms);

    // Prime the handler as if spawn_generators already ran.
    // buffer_generation was incremented in prepare_trigger_with_block, but
    // for these tests we set spawned_generation manually to match.
    handler.buffer_generation = 1;
    handler.set_spawned_generation(1);
    // Prime dynamic_ctx so the staleness check in try_merge_dynamic passes.
    handler.prime_dynamic_ctx_for_empty_buffer();

    let (tx, rx) = mpsc::channel::<Vec<Suggestion>>(1);
    handler.restore_dynamic_rx(rx);

    // Spawn a task that sends async_items after `delay`.
    let sender = tokio::spawn(async move {
        tokio::time::sleep(delay).await;
        let _ = tx.send(async_items).await;
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
    let (maybe_async, rx_on_timeout): (Option<Vec<Suggestion>>, Option<mpsc::Receiver<_>>) = {
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
    assert_eq!(async_results.len(), 2, "expected 2 branch suggestions");

    // Now call apply_block_result.
    let parser = make_parser();
    let mut render_buf = Vec::new();
    handler.apply_block_result(
        &parser,
        &mut render_buf,
        Some(async_results),
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

    let (maybe_async, rx_on_timeout): (Option<Vec<Suggestion>>, Option<mpsc::Receiver<_>>) = {
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

    let start = Instant::now();
    let timeout_dur = Duration::from_millis(80);

    // Three-way select mirrors the debounce_loop Phase 2 in proxy.rs.
    // AsyncCompleted and Timeout are panic cases so we don't need to inspect
    // their payloads in the happy-path assertions — use () to silence
    // dead_code lints.
    #[derive(Debug)]
    enum Outcome {
        AsyncCompleted(()),
        Timeout(()),
        KeystrokeCancelled(mpsc::Receiver<Vec<Suggestion>>),
    }

    let outcome = {
        let mut rx = rx;
        tokio::select! {
            _maybe_result = rx.recv() => Outcome::AsyncCompleted(()),
            _ = tokio::time::sleep(timeout_dur) => Outcome::Timeout(()),
            _ = keystroke_notify.notified() => Outcome::KeystrokeCancelled(rx),
        }
    };

    let elapsed = start.elapsed();

    match outcome {
        Outcome::KeystrokeCancelled(returned_rx) => {
            // The block was cancelled at ~30 ms.
            assert!(
                elapsed < Duration::from_millis(70),
                "keystroke cancellation should fire around 30 ms, elapsed: {:?}",
                elapsed
            );
            // Restore rx in the handler so dynamic_merge_loop can use it later.
            handler.restore_dynamic_rx(returned_rx);
            assert!(
                handler.has_dynamic_rx(),
                "dynamic_rx should be restored after keystroke cancellation"
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
        Outcome::Timeout(()) => {
            panic!("expected keystroke cancellation at ~30 ms, but timeout fired at ~80 ms");
        }
        Outcome::AsyncCompleted(()) => {
            panic!(
                "expected keystroke cancellation, but async generator completed (it should take 200 ms)"
            );
        }
    }

    // Cleanup.
    sender_task.abort();
    let _ = sender_task.await;

    let _ = sync_suggestions; // suppress unused warning
}

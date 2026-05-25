use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use gc_overlay::{
    clear_detail_box, clear_popup_unframed, render_detail_box, render_popup, render_popup_unframed,
    with_overlay_update_frame, DetailLayout, DetailPosition, FeedbackKind, OverlayState,
    PopupLayout, PopupTheme,
};
use gc_suggest::{Suggestion, SuggestionKind, SuggestionSource};
use gc_terminal::TerminalProfile;
use std::hint::black_box;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn make_suggestion(text: &str, desc: Option<&str>, kind: SuggestionKind) -> Suggestion {
    Suggestion {
        text: text.to_string(),
        description: desc.map(String::from),
        kind,
        source: SuggestionSource::Spec,
        ..Default::default()
    }
}

fn make_suggestions(n: usize) -> Vec<Suggestion> {
    (0..n)
        .map(|i| {
            make_suggestion(
                &format!("suggestion-{i:04}"),
                Some("A description for benchmark item"),
                SuggestionKind::Subcommand,
            )
        })
        .collect()
}

fn bordered_theme() -> PopupTheme {
    PopupTheme {
        borders: true,
        ..PopupTheme::default()
    }
}

// ---------------------------------------------------------------------------
// bench_popup_render
// Framed render_popup with representative suggestion counts x 2 profiles
// x 2 border variants.
// ---------------------------------------------------------------------------

fn bench_popup_render(c: &mut Criterion) {
    let ghostty = TerminalProfile::for_ghostty();
    let iterm2 = TerminalProfile::for_iterm2();
    let state = OverlayState::new();
    // Themes are hoisted out of every `b.iter` closure: `PopupTheme::default()`
    // allocates ~8 `Vec<u8>` fields, and charging that to the timed region
    // would skew the measurement toward theme construction over render work.
    let default_theme = PopupTheme::default();
    let bordered = bordered_theme();

    let profiles: &[(&str, &TerminalProfile)] = &[("ghostty", &ghostty), ("iterm2", &iterm2)];
    // 10 exercises the sub-max_visible fast path; 100/500 confirm render
    // cost is O(max_visible), not O(n), once the list exceeds the visible
    // window (compute_layout only measures the visible slice).
    let counts = [10usize, 100, 500];

    let mut group = c.benchmark_group("bench_popup_render");

    for &count in &counts {
        let suggestions = make_suggestions(count);

        for (profile_name, profile) in profiles {
            // Unbordered
            group.bench_with_input(
                BenchmarkId::new(format!("{profile_name}/unbordered"), count),
                &suggestions,
                |b, sugs| {
                    b.iter(|| {
                        let mut buf = Vec::with_capacity(4096);
                        let layout = render_popup(
                            &mut buf,
                            black_box(sugs),
                            black_box(&state),
                            black_box(10u16),
                            black_box(0u16),
                            black_box(40u16),
                            black_box(120u16),
                            black_box(10usize),
                            black_box(20u16),
                            black_box(60u16),
                            black_box(&default_theme),
                            black_box(0u16),
                            black_box(FeedbackKind::None),
                            black_box(profile),
                        );
                        black_box(buf);
                        black_box(layout);
                    });
                },
            );

            // Bordered
            group.bench_with_input(
                BenchmarkId::new(format!("{profile_name}/bordered"), count),
                &suggestions,
                |b, sugs| {
                    b.iter(|| {
                        let mut buf = Vec::with_capacity(4096);
                        let layout = render_popup(
                            &mut buf,
                            black_box(sugs),
                            black_box(&state),
                            black_box(10u16),
                            black_box(0u16),
                            black_box(40u16),
                            black_box(120u16),
                            black_box(10usize),
                            black_box(20u16),
                            black_box(60u16),
                            black_box(&bordered),
                            black_box(0u16),
                            black_box(FeedbackKind::None),
                            black_box(profile),
                        );
                        black_box(buf);
                        black_box(layout);
                    });
                },
            );
        }
    }

    // Feedback-only (loading indicator, no suggestions)
    for (profile_name, profile) in profiles {
        group.bench_function(format!("{profile_name}/feedback_only_loading"), |b| {
            b.iter(|| {
                let mut buf = Vec::with_capacity(256);
                let layout = render_popup(
                    &mut buf,
                    black_box(&[]),
                    black_box(&state),
                    black_box(10u16),
                    black_box(0u16),
                    black_box(40u16),
                    black_box(120u16),
                    black_box(10usize),
                    black_box(20u16),
                    black_box(60u16),
                    black_box(&default_theme),
                    black_box(0u16),
                    black_box(FeedbackKind::Loading { frame: 3 }),
                    black_box(profile),
                );
                black_box(buf);
                black_box(layout);
            });
        });
    }

    group.finish();
}

// ---------------------------------------------------------------------------
// bench_detail_render
// render_detail_box with short / wrapped(long) / CJK / ANSI-containing
// descriptions.
// ---------------------------------------------------------------------------

fn bench_detail_render(c: &mut Criterion) {
    // A non-trivial layout that exercises real content rendering.
    let layout = DetailLayout {
        start_row: 6,
        start_col: 62,
        width: 40,
        height: 8,
        position: DetailPosition::SideRight,
    };
    let theme = PopupTheme::default();
    let bordered = bordered_theme();

    let short_desc = "Switch branches or restore working tree files.";
    let long_desc = "Switches branches or restores working tree files. This is a \
        very long description that will need to be word-wrapped across multiple \
        lines in the detail box. It exercises the wrap_description path.";
    let cjk_desc = "日本語の説明文です。これはテストです。Unicode幅の計算が正しく動作するかを確認するための文字列。";
    // Benign SGR sequences only (bold/red/reset) — exercises the same
    // `sanitize_display_text` strip path without a screen-clearing escape
    // (e.g. CSI 2J) sitting in the source fixture.
    let ansi_desc = "\x1b[1mThis description contains ANSI escape sequences \
        like \x1b[31mred text\x1b[0m that must be sanitized before rendering.";

    let mut group = c.benchmark_group("bench_detail_render");

    group.bench_function("short_unbordered", |b| {
        b.iter(|| {
            let mut buf = Vec::with_capacity(512);
            render_detail_box(
                &mut buf,
                black_box(&layout),
                black_box(short_desc),
                black_box(&theme),
            );
            black_box(buf);
        });
    });

    group.bench_function("long_wrapped_unbordered", |b| {
        b.iter(|| {
            let mut buf = Vec::with_capacity(1024);
            render_detail_box(
                &mut buf,
                black_box(&layout),
                black_box(long_desc),
                black_box(&theme),
            );
            black_box(buf);
        });
    });

    group.bench_function("cjk_unbordered", |b| {
        b.iter(|| {
            let mut buf = Vec::with_capacity(1024);
            render_detail_box(
                &mut buf,
                black_box(&layout),
                black_box(cjk_desc),
                black_box(&theme),
            );
            black_box(buf);
        });
    });

    group.bench_function("ansi_sanitized_unbordered", |b| {
        b.iter(|| {
            let mut buf = Vec::with_capacity(512);
            render_detail_box(
                &mut buf,
                black_box(&layout),
                black_box(ansi_desc),
                black_box(&theme),
            );
            black_box(buf);
        });
    });

    // Bordered variant
    let bordered_layout = DetailLayout {
        start_row: 6,
        start_col: 62,
        width: 40,
        height: 10,
        position: DetailPosition::SideRight,
    };

    group.bench_function("long_wrapped_bordered", |b| {
        b.iter(|| {
            let mut buf = Vec::with_capacity(1024);
            render_detail_box(
                &mut buf,
                black_box(&bordered_layout),
                black_box(long_desc),
                black_box(&bordered),
            );
            black_box(buf);
        });
    });

    group.finish();
}

// ---------------------------------------------------------------------------
// bench_full_update
// A full overlay update: clear old popup + clear old detail + render new
// popup + render new detail, all staged into one buffer inside
// with_overlay_update_frame. Uses unframed primitives inside one frame.
// ---------------------------------------------------------------------------

fn bench_full_update(c: &mut Criterion) {
    let ghostty = TerminalProfile::for_ghostty();
    let iterm2 = TerminalProfile::for_iterm2();
    let state = OverlayState::new();
    let theme = PopupTheme::default();
    let suggestions = make_suggestions(50);

    let profiles: &[(&str, &TerminalProfile)] = &[("ghostty", &ghostty), ("iterm2", &iterm2)];

    // Prior layout to clear
    let prior_popup = PopupLayout {
        start_row: 6,
        start_col: 0,
        width: 60,
        height: 10,
        scroll_deficit: 0,
    };
    let prior_detail = DetailLayout {
        start_row: 6,
        start_col: 62,
        width: 40,
        height: 8,
        position: DetailPosition::SideRight,
    };

    let description = "Switches branches or restores working tree files. A longer description \
        that exercises word wrapping in the detail box render path.";

    let mut group = c.benchmark_group("bench_full_update");

    for (profile_name, profile) in profiles {
        group.bench_function(format!("{profile_name}/50_suggestions"), |b| {
            b.iter(|| {
                let mut buf = Vec::with_capacity(8192);
                with_overlay_update_frame(&mut buf, black_box(profile), |buf| {
                    // Clear prior surfaces
                    clear_popup_unframed(buf, black_box(&prior_popup));
                    clear_detail_box(buf, black_box(&prior_detail));

                    // Render new popup (unframed — we own the outer frame)
                    let new_layout = render_popup_unframed(
                        buf,
                        black_box(&suggestions),
                        black_box(&state),
                        black_box(10u16),
                        black_box(0u16),
                        black_box(40u16),
                        black_box(120u16),
                        black_box(10usize),
                        black_box(20u16),
                        black_box(60u16),
                        black_box(&theme),
                        black_box(0u16),
                        black_box(FeedbackKind::Loading { frame: 1 }),
                    );

                    // Render new detail box (already unframed)
                    let detail_layout = DetailLayout {
                        start_row: new_layout.start_row,
                        start_col: new_layout.start_col + new_layout.width + 1,
                        width: 40,
                        height: 8,
                        position: DetailPosition::SideRight,
                    };
                    render_detail_box(
                        buf,
                        black_box(&detail_layout),
                        black_box(description),
                        black_box(&theme),
                    );

                    black_box(new_layout);
                });
                black_box(buf);
            });
        });
    }

    group.finish();
}

criterion_group!(
    benches,
    bench_popup_render,
    bench_detail_render,
    bench_full_update
);
criterion_main!(benches);

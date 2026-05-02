use criterion::{criterion_group, criterion_main, Criterion};
use gc_overlay::{render_indicator_row, FeedbackKind, PopupLayout, PopupTheme};
use std::hint::black_box;

fn feedback_render(c: &mut Criterion) {
    let layout = PopupLayout {
        start_row: 5,
        start_col: 0,
        width: 60,
        height: 4,
        scroll_deficit: 0,
    };
    let theme = PopupTheme::default();

    c.bench_function("feedback_render/indicator_row_width_60", |b| {
        b.iter(|| {
            let mut buf = Vec::with_capacity(128);
            render_indicator_row(
                &mut buf,
                black_box(&layout),
                black_box(&theme),
                black_box(FeedbackKind::Loading { frame: 3 }),
            );
            black_box(buf);
        });
    });

    c.bench_function(
        "feedback_render/indicator_row_width_60_varying_frame",
        |b| {
            let mut frame: u8 = 0;
            b.iter(|| {
                let mut buf = Vec::with_capacity(128);
                let f = black_box(frame);
                render_indicator_row(
                    &mut buf,
                    black_box(&layout),
                    black_box(&theme),
                    black_box(FeedbackKind::Loading { frame: f }),
                );
                black_box(buf);
                frame = frame.wrapping_add(1);
            });
        },
    );
}

criterion_group!(benches, feedback_render);
criterion_main!(benches);

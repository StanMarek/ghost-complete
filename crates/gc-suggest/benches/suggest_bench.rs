use std::path::Path;

use criterion::{criterion_group, criterion_main, Criterion};

use gc_suggest::fuzzy;
use gc_suggest::types::{Suggestion, SuggestionKind, SuggestionSource};
use gc_suggest::SpecStore;

fn make_suggestion(text: &str) -> Suggestion {
    Suggestion {
        text: text.to_string(),
        description: None,
        kind: SuggestionKind::Command,
        source: SuggestionSource::Commands,
        score: 0,
    }
}

fn generate_candidates(n: usize) -> Vec<Suggestion> {
    (0..n)
        .map(|i| make_suggestion(&format!("file_{i}.txt")))
        .collect()
}

fn fuzzy_benchmarks(c: &mut Criterion) {
    let mut group = c.benchmark_group("fuzzy_ranking");

    group.bench_function("1k_3char", |b| {
        let candidates = generate_candidates(1_000);
        b.iter(|| fuzzy::rank("fil", candidates.clone(), fuzzy::DEFAULT_MAX_RESULTS));
    });

    group.bench_function("10k_3char", |b| {
        let candidates = generate_candidates(10_000);
        b.iter(|| fuzzy::rank("fil", candidates.clone(), fuzzy::DEFAULT_MAX_RESULTS));
    });

    group.bench_function("10k_empty", |b| {
        let candidates = generate_candidates(10_000);
        b.iter(|| fuzzy::rank("", candidates.clone(), fuzzy::DEFAULT_MAX_RESULTS));
    });

    group.finish();
}

fn spec_benchmarks(c: &mut Criterion) {
    let spec_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../specs");

    let mut group = c.benchmark_group("spec_loading");

    group.bench_function("load_717_specs", |b| {
        b.iter(|| SpecStore::load_from_dir(&spec_dir).unwrap());
    });

    group.finish();
}

criterion_group!(benches, fuzzy_benchmarks, spec_benchmarks);
criterion_main!(benches);

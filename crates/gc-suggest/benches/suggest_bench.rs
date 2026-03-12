use criterion::{criterion_group, criterion_main};

fn fuzzy_benchmarks(c: &mut criterion::Criterion) {
    c.bench_function("fuzzy_ranking/placeholder", |b| {
        b.iter(|| 1 + 1)
    });
}

criterion_group!(benches, fuzzy_benchmarks);
criterion_main!(benches);

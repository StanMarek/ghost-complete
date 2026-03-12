use criterion::{criterion_group, criterion_main};

fn parser_benchmarks(c: &mut criterion::Criterion) {
    c.bench_function("vt_parse_throughput/placeholder", |b| {
        b.iter(|| 1 + 1)
    });
}

criterion_group!(benches, parser_benchmarks);
criterion_main!(benches);

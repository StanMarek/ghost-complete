use criterion::{criterion_group, criterion_main, Criterion, Throughput};
use gc_parser::TerminalParser;

const BUFFER_SIZE: usize = 64 * 1024; // 64 KB

fn make_plain_text(size: usize) -> Vec<u8> {
    let line = b"drwxr-xr-x  5 user staff  160 Mar 12 10:00 dirname\n";
    let mut buf = Vec::with_capacity(size);
    while buf.len() < size {
        let remaining = size - buf.len();
        let chunk = &line[..remaining.min(line.len())];
        buf.extend_from_slice(chunk);
    }
    buf.truncate(size);
    buf
}

fn make_ansi_colored(size: usize) -> Vec<u8> {
    let line = b"\x1b[32m+added line\x1b[0m\n\x1b[31m-removed line\x1b[0m\n";
    let mut buf = Vec::with_capacity(size);
    while buf.len() < size {
        let remaining = size - buf.len();
        let chunk = &line[..remaining.min(line.len())];
        buf.extend_from_slice(chunk);
    }
    buf.truncate(size);
    buf
}

fn make_cursor_heavy(size: usize) -> Vec<u8> {
    let seq = b"\x1b[H\x1b[2J\x1b[10;20Htext\x1b[1A\x1b[5C";
    let mut buf = Vec::with_capacity(size);
    while buf.len() < size {
        let remaining = size - buf.len();
        let chunk = &seq[..remaining.min(seq.len())];
        buf.extend_from_slice(chunk);
    }
    buf.truncate(size);
    buf
}

fn parser_benchmarks(c: &mut Criterion) {
    let mut group = c.benchmark_group("vt_parse_throughput");
    group.throughput(Throughput::Bytes(BUFFER_SIZE as u64));

    let plain = make_plain_text(BUFFER_SIZE);
    group.bench_function("plain_text", |b| {
        b.iter(|| {
            let mut parser = TerminalParser::new(24, 80);
            parser.process_bytes(&plain);
        });
    });

    let colored = make_ansi_colored(BUFFER_SIZE);
    group.bench_function("ansi_colored", |b| {
        b.iter(|| {
            let mut parser = TerminalParser::new(24, 80);
            parser.process_bytes(&colored);
        });
    });

    let cursor = make_cursor_heavy(BUFFER_SIZE);
    group.bench_function("cursor_heavy", |b| {
        b.iter(|| {
            let mut parser = TerminalParser::new(24, 80);
            parser.process_bytes(&cursor);
        });
    });

    group.finish();
}

criterion_group!(benches, parser_benchmarks);
criterion_main!(benches);

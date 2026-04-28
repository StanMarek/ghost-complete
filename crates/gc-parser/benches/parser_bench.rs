use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use gc_parser::TerminalParser;

const BUFFER_SIZE: usize = 64 * 1024; // 64 KB

const HEX: &[u8; 16] = b"0123456789ABCDEF";

/// Mirror `_gc_urlencode_buffer` (zsh) byte-for-byte. Bench numbers
/// describe the production path because the production path uses the
/// same encoding alphabet.
fn osc7772_encode(input: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(input.len() * 3);
    for &b in input {
        let safe = matches!(b,
            b'a'..=b'z'
                | b'A'..=b'Z'
                | b'0'..=b'9'
                | b'.' | b'_' | b'~' | b'/' | b'-' | b' '
        );
        if safe {
            out.push(b);
        } else {
            out.push(b'%');
            out.push(HEX[(b >> 4) as usize]);
            out.push(HEX[(b & 0x0F) as usize]);
        }
    }
    out
}

fn build_osc7772_envelope(buffer: &[u8]) -> Vec<u8> {
    let cursor = std::str::from_utf8(buffer)
        .map(|s| s.chars().count())
        .unwrap_or(buffer.len());
    let mut env = Vec::with_capacity(buffer.len() * 3 + 16);
    env.extend_from_slice(b"\x1b]7772;");
    env.extend_from_slice(cursor.to_string().as_bytes());
    env.push(b';');
    env.extend_from_slice(&osc7772_encode(buffer));
    env.push(0x07);
    env
}

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

fn osc7772_decode_benchmarks(c: &mut Criterion) {
    // Mixed alphabet: half the bytes are safe ASCII (pass-through), half
    // require percent-encoding. Realistic for shells that contain a
    // sprinkling of `;`, `|`, `&`, etc. Use a deterministic pattern so
    // bench runs are comparable across machines and revisions.
    let pattern: &[u8] = b"echo a; ls -la | grep -v test && cd /tmp ";
    let make_buffer = |size: usize| -> Vec<u8> {
        pattern.iter().cycle().take(size).copied().collect()
    };

    let mut group = c.benchmark_group("osc7772_decode");
    for &size in &[100usize, 1024, 8 * 1024] {
        let envelope = build_osc7772_envelope(&make_buffer(size));
        group.throughput(Throughput::Bytes(envelope.len() as u64));
        group.bench_with_input(
            BenchmarkId::from_parameter(size),
            &envelope,
            |b, env| {
                b.iter(|| {
                    let mut parser = TerminalParser::new(24, 80);
                    parser.process_bytes(env);
                });
            },
        );
    }
    group.finish();
}

criterion_group!(benches, parser_benchmarks, osc7772_decode_benchmarks);
criterion_main!(benches);

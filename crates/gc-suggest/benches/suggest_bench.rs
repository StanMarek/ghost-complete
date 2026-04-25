use std::path::Path;

use criterion::{criterion_group, criterion_main, Criterion};

use gc_buffer::{CommandContext, QuoteState};
use gc_suggest::commands::CommandsProvider;
use gc_suggest::fuzzy;
use gc_suggest::history::HistoryProvider;
use gc_suggest::specs;
use gc_suggest::transform::{self, NamedTransform, ParameterizedTransform, Transform};
use gc_suggest::types::Suggestion;
use gc_suggest::SpecStore;
use gc_suggest::SuggestionEngine;

fn make_suggestion(text: &str) -> Suggestion {
    Suggestion {
        text: text.to_string(),
        ..Default::default()
    }
}

fn generate_candidates(n: usize) -> Vec<Suggestion> {
    (0..n)
        .map(|i| make_suggestion(&format!("file_{i}.txt")))
        .collect()
}

fn make_ctx(
    command: Option<&str>,
    args: Vec<&str>,
    current_word: &str,
    word_index: usize,
) -> CommandContext {
    CommandContext {
        command: command.map(String::from),
        args: args.into_iter().map(String::from).collect(),
        current_word: current_word.to_string(),
        word_index,
        is_flag: current_word.starts_with('-'),
        is_long_flag: current_word.starts_with("--"),
        preceding_flag: None,
        in_pipe: false,
        in_redirect: false,
        quote_state: QuoteState::None,
        is_first_segment: true,
    }
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

fn resolution_benchmarks(c: &mut Criterion) {
    let spec_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../specs");
    let store = SpecStore::load_from_dir(&spec_dir).unwrap().store;

    let mut group = c.benchmark_group("spec_resolution");

    let git_spec = store.get("git").expect("git spec must exist");
    let shallow_ctx = make_ctx(Some("git"), vec!["checkout"], "", 2);
    group.bench_function("shallow", |b| {
        b.iter(|| specs::resolve_spec(git_spec, &shallow_ctx));
    });

    let docker_spec = store.get("docker").expect("docker spec must exist");
    let deep_ctx = make_ctx(Some("docker"), vec!["compose", "up"], "--", 3);
    group.bench_function("deep", |b| {
        b.iter(|| specs::resolve_spec(docker_spec, &deep_ctx));
    });

    group.finish();
}

fn transform_benchmarks(c: &mut Criterion) {
    let mut group = c.benchmark_group("transform_pipeline");

    // Simple: split_lines + filter_empty + trim on 500 lines
    let simple_input: String = (0..500).map(|i| format!("  line_{i}  \n")).collect();
    let simple_transforms = vec![
        Transform::Named(NamedTransform::SplitLines),
        Transform::Named(NamedTransform::FilterEmpty),
        Transform::Named(NamedTransform::Trim),
    ];
    group.bench_function("simple", |b| {
        b.iter(|| transform::execute_pipeline(&simple_input, &simple_transforms).unwrap());
    });

    // Regex: split_lines + regex_extract on git-branch-like output
    let regex_input: String = (0..200)
        .map(|i| format!("  branch_{i}  abc1234 commit message {i}\n"))
        .collect();
    let regex_transforms = vec![
        Transform::Named(NamedTransform::SplitLines),
        Transform::Parameterized(ParameterizedTransform::RegexExtract {
            compiled: regex::Regex::new(r"^\s*(\S+)\s+(\S+)\s+(.+)$").unwrap(),
            name: 1,
            description: Some(3),
        }),
    ];
    group.bench_function("regex", |b| {
        b.iter(|| transform::execute_pipeline(&regex_input, &regex_transforms).unwrap());
    });

    // JSON: split_lines + json_extract on 100 JSON-per-line objects
    let json_input: String = (0..100)
        .map(|i| format!("{{\"name\":\"item_{i}\",\"desc\":\"description {i}\"}}\n"))
        .collect();
    let json_transforms = vec![
        Transform::Named(NamedTransform::SplitLines),
        Transform::Parameterized(ParameterizedTransform::JsonExtract {
            name: gc_suggest::JsonPath::parse("name").unwrap(),
            description: Some(gc_suggest::JsonPath::parse("desc").unwrap()),
        }),
    ];
    group.bench_function("json", |b| {
        b.iter(|| transform::execute_pipeline(&json_input, &json_transforms).unwrap());
    });

    group.finish();
}

fn setup_engine_and_dir() -> (SuggestionEngine, tempfile::TempDir) {
    let spec_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../specs");
    let store = SpecStore::load_from_dir(&spec_dir).unwrap().store;

    let commands: Vec<String> = (0..100).map(|i| format!("cmd_{i}")).collect();
    let commands_provider = CommandsProvider::from_list(commands);

    let history: Vec<String> = (0..50)
        .map(|i| format!("git push origin branch_{i}"))
        .collect();
    let history_provider = HistoryProvider::from_entries(history);

    let engine = SuggestionEngine::with_providers(store, history_provider, commands_provider);

    let tmp = tempfile::TempDir::new().unwrap();
    for i in 0..1500 {
        std::fs::write(tmp.path().join(format!("file_{i}.rs")), "").unwrap();
    }
    for i in 0..300 {
        std::fs::write(tmp.path().join(format!("data_{i}.json")), "").unwrap();
    }
    for i in 0..200 {
        let dir = tmp.path().join(format!("dir_{i}"));
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(dir.join("mod.rs"), "").unwrap();
    }

    (engine, tmp)
}

fn engine_benchmarks(c: &mut Criterion) {
    let (engine, tmp) = setup_engine_and_dir();

    let mut group = c.benchmark_group("engine_suggest_sync");

    let cmd_ctx = make_ctx(None, vec![], "gi", 0);
    group.bench_function("command_position", |b| {
        b.iter(|| engine.suggest_sync(&cmd_ctx, tmp.path(), "gi").unwrap());
    });

    let sub_ctx = make_ctx(Some("git"), vec![], "ch", 1);
    group.bench_function("subcommand_with_spec", |b| {
        b.iter(|| engine.suggest_sync(&sub_ctx, tmp.path(), "git ch").unwrap());
    });

    let fs_ctx = make_ctx(Some("unknown_cmd_xyz"), vec![], "", 1);
    group.bench_function("filesystem_fallback", |b| {
        b.iter(|| {
            engine
                .suggest_sync(&fs_ctx, tmp.path(), "unknown_cmd_xyz ")
                .unwrap()
        });
    });

    group.finish();
}

criterion_group!(
    benches,
    fuzzy_benchmarks,
    spec_benchmarks,
    resolution_benchmarks,
    transform_benchmarks,
    engine_benchmarks,
);
criterion_main!(benches);

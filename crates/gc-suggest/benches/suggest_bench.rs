use std::path::Path;

use criterion::{criterion_group, criterion_main, Criterion};

use gc_buffer::{CommandContext, QuoteState};
use gc_suggest::commands::CommandsProvider;
use gc_suggest::fuzzy;
use gc_suggest::history::HistoryProvider;
use gc_suggest::priority::Priority;
use gc_suggest::specs;
use gc_suggest::transform::{self, NamedTransform, ParameterizedTransform, Transform};
use gc_suggest::types::{Suggestion, SuggestionKind};
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
        b.iter(|| specs::resolve_spec(git_spec.as_ref(), &shallow_ctx));
    });

    let docker_spec = store.get("docker").expect("docker spec must exist");
    let deep_ctx = make_ctx(Some("docker"), vec!["compose", "up"], "--", 3);
    group.bench_function("deep", |b| {
        b.iter(|| specs::resolve_spec(docker_spec.as_ref(), &deep_ctx));
    });

    // tar c --atime-preserve <TAB> — exercises the preceding_flag path with
    // static suggestions populated.
    let tar_spec = store.get("tar").expect("tar spec must exist");
    let static_ctx = CommandContext {
        command: Some("tar".into()),
        args: vec!["c".into(), "--atime-preserve".into()],
        current_word: String::new(),
        word_index: 3,
        is_flag: false,
        is_long_flag: false,
        preceding_flag: Some("--atime-preserve".into()),
        in_pipe: false,
        in_redirect: false,
        quote_state: QuoteState::None,
        is_first_segment: true,
    };
    group.bench_function("with_static_suggestions_tar", |b| {
        b.iter(|| specs::resolve_spec(tar_spec.as_ref(), &static_ctx));
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

    // Two cases isolate alias expansion overhead vs the no-alias subcommand bench.
    let (alias_engine, alias_tmp) = setup_engine_and_dir();
    let mut alias_map = std::collections::HashMap::with_capacity(100);
    alias_map.insert("g".to_string(), vec!["git".to_string()]);
    alias_map.insert(
        "gco".to_string(),
        vec!["git".to_string(), "checkout".to_string()],
    );
    for i in 0..98 {
        alias_map.insert(
            format!("alias_{i}"),
            vec![format!("cmd_{i}"), format!("sub_{i}")],
        );
    }
    let alias_engine = alias_engine.with_aliases(alias_map);

    let alias_ctx_single = make_ctx(Some("g"), vec![], "ch", 1);
    group.bench_function("alias_hit_single", |b| {
        b.iter(|| {
            alias_engine
                .suggest_sync(&alias_ctx_single, alias_tmp.path(), "g ch")
                .unwrap()
        });
    });

    let alias_ctx_multi = make_ctx(Some("gco"), vec![], "m", 1);
    group.bench_function("alias_hit_multi", |b| {
        b.iter(|| {
            alias_engine
                .suggest_sync(&alias_ctx_multi, alias_tmp.path(), "gco m")
                .unwrap()
        });
    });

    group.finish();
}

fn token_only_engine(source: &str) -> (SuggestionEngine, tempfile::TempDir) {
    let temp = tempfile::TempDir::new().unwrap();
    let escaped_source = serde_json::to_string(source).unwrap();
    let spec = format!(
        r#"{{
            "name": "bench-token-only",
            "args": [{{
                "name": "value",
                "generators": [{{
                    "requires_js": true,
                    "js_runtime": {{
                        "kind": "token_only",
                        "source": {escaped_source},
                        "self_contained": false
                    }}
                }}]
            }}]
        }}"#
    );
    std::fs::write(temp.path().join("bench-token-only.json"), spec).unwrap();
    let store = SpecStore::load_from_dir(temp.path()).unwrap().store;
    let engine = SuggestionEngine::with_providers(
        store,
        HistoryProvider::from_entries(Vec::new()),
        CommandsProvider::from_list(Vec::new()),
    );
    (engine, temp)
}

fn token_only_benchmarks(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let ctx = make_ctx(Some("bench-token-only"), vec!["get"], "po", 2);

    let mut group = c.benchmark_group("engine_suggest_sync_token_only");
    for (name, source) in [
        ("returns_tokens", "tokens.map(name => ({ name }))"),
        (
            "previous_token_branch",
            "previousToken === 'get' ? ['pods', 'services'].map(name => ({ name })) : ['apply'].map(name => ({ name }))",
        ),
        (
            "regex_filter",
            "['pods', 'services', 'deployments'].filter(name => /^p/.test(name)).map(name => ({ name }))",
        ),
    ] {
        let (engine, temp) = token_only_engine(source);
        let scheduled = engine
            .suggest_sync(&ctx, temp.path(), "bench-token-only get po")
            .unwrap()
            .script_generators;
        group.bench_function(name, |b| {
            b.iter(|| {
                let suggestions = rt
                    .block_on(engine.run_generators(&scheduled, &ctx, temp.path(), 50))
                    .unwrap();
                std::hint::black_box(suggestions);
            });
        });
    }
    group.finish();
}

fn priority_sort_benchmarks(c: &mut Criterion) {
    let mut suggestions = Vec::with_capacity(10_000);
    for i in 0..10_000 {
        let kind = match i % 5 {
            0 => SuggestionKind::GitBranch,
            1 => SuggestionKind::Flag,
            2 => SuggestionKind::FilePath,
            3 => SuggestionKind::Subcommand,
            _ => SuggestionKind::ProviderValue,
        };
        let priority = if i % 50 == 0 {
            Some(Priority::new(95))
        } else {
            None
        };
        suggestions.push(Suggestion {
            text: format!("item-{i:05}"),
            kind,
            priority,
            ..Default::default()
        });
    }

    let mut group = c.benchmark_group("priority_sort");
    group.bench_function("empty_query_10k", |b| {
        b.iter(|| fuzzy::rank("", suggestions.clone(), fuzzy::DEFAULT_MAX_RESULTS));
    });
    group.bench_function("fuzzy_query_10k", |b| {
        b.iter(|| fuzzy::rank("item-007", suggestions.clone(), fuzzy::DEFAULT_MAX_RESULTS));
    });
    group.finish();
}

fn memory_benchmarks(c: &mut Criterion) {
    let spec_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../specs");
    let store = SpecStore::load_from_dir(&spec_dir).unwrap().store;
    let mut group = c.benchmark_group("memory");
    group.bench_function("embedded_specs_heap_walk", |b| {
        b.iter(|| {
            let total: usize = store
                .iter()
                .map(|(_, s)| specs::estimated_heap_bytes(s.as_ref()))
                .sum();
            std::hint::black_box(total)
        });
    });
    group.finish();
}

/// Lazy-loading benchmarks pinning the lazy-loading contract:
///
/// - `load_with_embedded_no_parse` quantifies the cost of registering the
///   entire embedded corpus as `SpecSource::Embedded` entries without
///   parsing any spec body. A regression here usually means an upstream
///   caller force-loaded specs at startup.
/// - `first_get_git` measures registration plus the first full parse of a
///   small embedded spec.
/// - `first_get_aws` measures the same for the largest spec in the corpus
///   (~36 MB minified, 17 K subcommands). This is the dominant cost users
///   pay when they first type `aws ` — it's ~exactly the work the eager
///   loader was doing for *every* spec at startup pre-fix.
/// - `warm_get_git` measures the steady-state lookup cost after the
///   `ParsedSlot` is populated. This is what users pay on every subsequent
///   lookup of the same spec.
fn lazy_load_benchmarks(c: &mut Criterion) {
    let mut group = c.benchmark_group("lazy_load");

    group.bench_function("load_with_embedded_no_parse", |b| {
        b.iter(|| {
            let result = SpecStore::load_with_embedded(&[]).expect("embedded corpus must load");
            // Hold the store across the iter() call so the compiler can't
            // elide the load. Crucially we do NOT iterate — that would
            // force-load every entry and conflate the lazy contract with
            // the parse cost.
            std::hint::black_box(result.store);
        });
    });

    // First-touch parse benchmarks: each iter() rebuilds the store so
    // every iteration measures the cost of registering + first-touching
    // a single spec from scratch. Without rebuilding we'd hit the
    // parse slot after iter #1 and report the warm-path latency.
    group.bench_function("first_get_git", |b| {
        b.iter(|| {
            let result = SpecStore::load_with_embedded(&[]).expect("embedded corpus must load");
            let store = result.store;
            let spec = store.get("git").expect("git spec must resolve");
            std::hint::black_box(spec);
        });
    });

    group.bench_function("first_get_aws", |b| {
        b.iter(|| {
            let result = SpecStore::load_with_embedded(&[]).expect("embedded corpus must load");
            let store = result.store;
            let spec = store.get("aws").expect("aws spec must resolve");
            std::hint::black_box(spec);
        });
    });

    // Warm path: build the store once outside the timed loop and time
    // only the lookup. The ParsedSlot is populated by the first iter, every
    // subsequent iter takes the fast path.
    let warm_result =
        SpecStore::load_with_embedded(&[]).expect("embedded corpus must load for warm bench");
    let warm_store = warm_result.store;
    // Prime the cache for git so the first iter doesn't pay the parse cost.
    let _ = warm_store.get("git");
    group.bench_function("warm_get_git", |b| {
        b.iter(|| {
            let spec = warm_store.get("git").expect("git spec must resolve");
            std::hint::black_box(spec);
        });
    });

    group.bench_function("evict_then_get_aws", |b| {
        b.iter_custom(|iters| {
            let mut total = std::time::Duration::ZERO;
            let result = SpecStore::load_with_embedded(&[]).expect("embedded corpus must load");
            let store = result.store;
            for _ in 0..iters {
                let _ = store.get("aws"); // ensure Loaded
                let entry = store
                    .entries()
                    .iter()
                    .find(|e| e.id == "aws")
                    .unwrap()
                    .clone();
                entry.set_last_accessed_for_test(
                    std::time::SystemTime::now() - std::time::Duration::from_secs(3600),
                );
                let _ = store.evict_idle(
                    std::time::Duration::from_secs(60),
                    None,
                    &std::collections::HashSet::new(),
                );
                let start = std::time::Instant::now();
                let spec = store
                    .get("aws")
                    .expect("aws must re-resolve after eviction");
                total += start.elapsed();
                std::hint::black_box(spec);
            }
            total
        });
    });

    group.bench_function("evict_idle_700_specs_no_op", |b| {
        let result = SpecStore::load_with_embedded(&[]).expect("embedded corpus must load");
        let store = result.store;
        // Force every entry through Loaded; bump timestamps to "just now".
        for (id, _) in store.iter() {
            let _ = id;
        }
        let keep_warm = std::collections::HashSet::new();
        b.iter(|| {
            let report = store.evict_idle(std::time::Duration::from_secs(3600), None, &keep_warm);
            std::hint::black_box(report);
        });
    });

    group.bench_function("evict_idle_700_specs_evict_all", |b| {
        // Re-build per iter so each iter starts from "everything Loaded".
        b.iter_custom(|iters| {
            let mut total = std::time::Duration::ZERO;
            for _ in 0..iters {
                let result = SpecStore::load_with_embedded(&[]).expect("embedded corpus must load");
                let store = result.store;
                for (_, _) in store.iter() {} // force-load all
                let keep_warm = std::collections::HashSet::new();
                let start = std::time::Instant::now();
                let _ = store.evict_idle(std::time::Duration::ZERO, None, &keep_warm);
                total += start.elapsed();
            }
            total
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
    token_only_benchmarks,
    priority_sort_benchmarks,
    memory_benchmarks,
    lazy_load_benchmarks,
);
criterion_main!(benches);

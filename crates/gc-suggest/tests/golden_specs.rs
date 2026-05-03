//! Golden snapshot tests for the high-traffic commands covered by the
//! ranking + suppression contract: `git checkout`, `cargo run`, `cd`,
//! `git checkout ./`, `git checkout --`, `npm install`, `docker run`,
//! `ssh`, and `kubectl get`. Invariants are documented in
//! `docs/superpowers/specs/2026-04-25-completion-ranking-and-suppression-design.md`.
//!
//! Each test feeds a canonical buffer into `SuggestionEngine::suggest_sync`
//! and asserts an invariant about the resulting suggestions — usually that
//! certain kinds appear at the top, that filesystem entries are absent
//! entirely (no fs leak under spec-driven contexts), or that priorities are
//! honoured when they disagree with the alphabetical fallback. Intentionally
//! checks kinds and ordinal positions (not exact text) so the tests survive
//! spec content changes upstream.

use std::path::{Path, PathBuf};

use gc_buffer::parse_command_context;
use gc_suggest::commands::CommandsProvider;
use gc_suggest::history::HistoryProvider;
use gc_suggest::specs::SpecStore;
use gc_suggest::types::Suggestion;
use gc_suggest::{SuggestionEngine, SuggestionKind};

fn spec_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../specs")
}

fn build_engine() -> SuggestionEngine {
    let spec_store = SpecStore::load_from_dir(&spec_dir()).unwrap().store;
    let history = HistoryProvider::from_entries(vec![]);
    let commands = CommandsProvider::from_list(vec![]);
    SuggestionEngine::with_providers(spec_store, history, commands)
}

fn ctx_from(buffer: &str) -> gc_buffer::CommandContext {
    parse_command_context(buffer, buffer.chars().count())
}

fn tmp_cwd() -> PathBuf {
    let dir = std::env::temp_dir().join("ghost-complete-golden-tests");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

#[test]
fn git_checkout_no_query_ranks_branches_first() {
    let engine = build_engine();
    let buffer = "git checkout ";
    let ctx = ctx_from(buffer);
    let result = engine.suggest_sync(&ctx, &tmp_cwd(), buffer).unwrap();
    assert!(
        result
            .suggestions
            .iter()
            .take(3)
            .any(|s| s.kind == SuggestionKind::GitBranch)
            || !result.git_generators.is_empty(),
        "branches should be either visible or pending"
    );
}

#[test]
fn git_branch_priority_outranks_flag_priority() {
    // GitBranch base 80 > Flag base 30 — branches must sort above flags
    // when both share the empty-query path.
    let items = vec![
        Suggestion {
            text: "--force".to_string(),
            kind: SuggestionKind::Flag,
            priority: None,
            ..Default::default()
        },
        Suggestion {
            text: "main".to_string(),
            kind: SuggestionKind::GitBranch,
            priority: None,
            ..Default::default()
        },
    ];
    let result = gc_suggest::fuzzy::rank("", items, 50);
    assert_eq!(result[0].kind, SuggestionKind::GitBranch);
}

#[test]
fn cargo_run_no_query_no_filesystem_leak() {
    let engine = build_engine();
    let buffer = "cargo run ";
    let ctx = ctx_from(buffer);
    let result = engine.suggest_sync(&ctx, &tmp_cwd(), buffer).unwrap();
    assert!(
        !result
            .suggestions
            .iter()
            .any(|s| matches!(s.kind, SuggestionKind::FilePath | SuggestionKind::Directory)),
        "cargo run should not leak filesystem; got {:?}",
        result
            .suggestions
            .iter()
            .map(|s| (&s.text, s.kind))
            .collect::<Vec<_>>()
    );
}

#[test]
fn cd_no_query_returns_directories_only() {
    let engine = build_engine();
    let buffer = "cd ";
    let ctx = ctx_from(buffer);
    let result = engine.suggest_sync(&ctx, &tmp_cwd(), buffer).unwrap();
    assert!(
        result
            .suggestions
            .iter()
            .all(|s| !matches!(s.kind, SuggestionKind::FilePath)),
        "cd should not return plain files; got {:?}",
        result
            .suggestions
            .iter()
            .map(|s| (&s.text, s.kind))
            .collect::<Vec<_>>()
    );
}

#[test]
fn git_checkout_path_prefix_returns_filesystem_only() {
    let engine = build_engine();
    let buffer = "git checkout ./";
    let ctx = ctx_from(buffer);
    let result = engine.suggest_sync(&ctx, &tmp_cwd(), buffer).unwrap();
    assert!(
        result
            .suggestions
            .iter()
            .all(|s| matches!(s.kind, SuggestionKind::FilePath | SuggestionKind::Directory)),
        "PathPrefix context should yield filesystem only; got {:?}",
        result
            .suggestions
            .iter()
            .map(|s| (&s.text, s.kind))
            .collect::<Vec<_>>()
    );
}

#[test]
fn git_checkout_flag_prefix_returns_flags_only() {
    let engine = build_engine();
    let buffer = "git checkout --";
    let ctx = ctx_from(buffer);
    let result = engine.suggest_sync(&ctx, &tmp_cwd(), buffer).unwrap();
    assert!(
        result
            .suggestions
            .iter()
            .all(|s| matches!(s.kind, SuggestionKind::Flag | SuggestionKind::Subcommand)),
        "FlagPrefix context should yield flags/subcommands only; got {:?}",
        result
            .suggestions
            .iter()
            .map(|s| (&s.text, s.kind))
            .collect::<Vec<_>>()
    );
}

#[test]
fn npm_install_no_filesystem_when_spec_omits_template() {
    let engine = build_engine();
    let buffer = "npm install ";
    let ctx = ctx_from(buffer);
    let result = engine.suggest_sync(&ctx, &tmp_cwd(), buffer).unwrap();
    assert!(
        !result
            .suggestions
            .iter()
            .any(|s| matches!(s.kind, SuggestionKind::FilePath | SuggestionKind::Directory)),
        "npm install spec lists no template; filesystem should not appear; got {:?}",
        result
            .suggestions
            .iter()
            .map(|s| (&s.text, s.kind))
            .collect::<Vec<_>>()
    );
}

#[test]
fn docker_run_no_filesystem_when_spec_omits_template() {
    let engine = build_engine();
    let buffer = "docker run ";
    let ctx = ctx_from(buffer);
    let result = engine.suggest_sync(&ctx, &tmp_cwd(), buffer).unwrap();
    assert!(
        !result
            .suggestions
            .iter()
            .any(|s| matches!(s.kind, SuggestionKind::FilePath | SuggestionKind::Directory)),
        "docker run image arg has script generator; no fs leak expected; got {:?}",
        result
            .suggestions
            .iter()
            .map(|s| (&s.text, s.kind))
            .collect::<Vec<_>>()
    );
}

#[test]
fn ssh_returns_hosts_or_pending_no_filesystem_leak() {
    let engine = build_engine();
    let buffer = "ssh ";
    let ctx = ctx_from(buffer);
    let result = engine.suggest_sync(&ctx, &tmp_cwd(), buffer).unwrap();
    assert!(
        !result
            .suggestions
            .iter()
            .any(|s| matches!(s.kind, SuggestionKind::FilePath | SuggestionKind::Directory)),
        "ssh arg has host generators; no fs leak expected; got {:?}",
        result
            .suggestions
            .iter()
            .map(|s| (&s.text, s.kind))
            .collect::<Vec<_>>()
    );
}

#[test]
fn kubectl_get_no_filesystem_when_spec_provides_generators() {
    let engine = build_engine();
    let buffer = "kubectl get ";
    let ctx = ctx_from(buffer);
    let result = engine.suggest_sync(&ctx, &tmp_cwd(), buffer).unwrap();
    assert!(
        !result
            .suggestions
            .iter()
            .any(|s| matches!(s.kind, SuggestionKind::FilePath | SuggestionKind::Directory)),
        "kubectl get resource arg has generators; no fs leak expected; got {:?}",
        result
            .suggestions
            .iter()
            .map(|s| (&s.text, s.kind))
            .collect::<Vec<_>>()
    );
}

/// The audit pass set `cargo run` to priority 92 and `cargo add` to 90.
/// With an empty query the ranker has no fuzzy signal — only kind/priority,
/// then alphabetical tiebreak. Alphabetically `add` precedes `run`, so if
/// the priority plumbing were ever broken (both subcommands falling back to
/// the Subcommand kind base of 70 with no override), `add` would land
/// before `run`. Picking a pair where alphabetical order is REVERSED by
/// priority means this assertion only passes when priorities are actually
/// honoured end-to-end.
#[test]
fn cargo_high_priority_subcommand_outranks_alphabetical_neighbour() {
    let engine = build_engine();
    let buffer = "cargo ";
    let ctx = ctx_from(buffer);
    let result = engine.suggest_sync(&ctx, &tmp_cwd(), buffer).unwrap();

    let position = |name: &str| {
        result
            .suggestions
            .iter()
            .position(|s| s.text == name && s.kind == SuggestionKind::Subcommand)
    };

    let run_pos = position("run").expect("`cargo run` subcommand should be suggested");
    let add_pos = position("add").expect("`cargo add` subcommand should be suggested");

    assert!(
        run_pos < add_pos,
        "cargo run (priority 92) must rank before cargo add (priority 90); \
         alphabetical fallback would put `add` first, so this only holds \
         when priorities are honoured. got run at {run_pos}, add at {add_pos}: {:?}",
        result
            .suggestions
            .iter()
            .map(|s| (&s.text, s.kind, s.priority))
            .collect::<Vec<_>>()
    );
}

// ---------- AWS spec guardrails (ux-8) ----------
//
// The AWS spec is a 36 MB minified blob produced by `npm run convert -- --specs aws`,
// inlining 418 service sub-specs from @withfig/autocomplete via the converter's
// loadSpec resolver. The snapshot gate proves the artifact equals itself; these
// tests prove the artifact has the *shape* the runtime expects.
//
// Assertions are tolerant to upstream churn — we don't pin exact subcommand
// counts because @withfig/autocomplete deprecates services occasionally. We
// pin the names of services / actions / flags that are essentially immortal
// in the AWS CLI (s3 cp, ec2 describe-instances, the global --profile option).
// If any of these stop existing, somebody removed them on purpose and the
// test should fail loudly so we notice.

fn load_aws_spec() -> gc_suggest::specs::CompletionSpec {
    SpecStore::load_from_dir(&spec_dir())
        .unwrap()
        .store
        .get("aws")
        .cloned()
        .expect("aws spec must be present in specs/ — restored in ux-8")
}

#[test]
fn aws_spec_top_level_has_many_service_subcommands() {
    // The converter inlines @withfig/autocomplete's 418 service sub-specs as
    // top-level subcommands of `aws`. We assert "lots of services" rather
    // than an exact count so a single deprecation upstream doesn't fail CI.
    let aws = load_aws_spec();
    let count = aws.subcommands.len();
    assert!(
        (350..=500).contains(&count),
        "expected aws to ship in the 350..=500 service range; got {count} \
         (if upstream pruned a lot, widen the range; if it grew past 500, \
          revisit the binary-size budget)"
    );
}

#[test]
fn aws_spec_includes_canonical_services() {
    // s3, ec2, iam, lambda, sts: the five services every AWS user touches.
    // If any of these vanish, something broke in the loadSpec resolver.
    let aws = load_aws_spec();
    let services: std::collections::HashSet<&str> =
        aws.subcommands.iter().map(|s| s.name.as_str()).collect();
    for must_have in ["s3", "ec2", "iam", "lambda", "sts"] {
        assert!(
            services.contains(must_have),
            "aws is missing service `{must_have}`; loadSpec resolver \
             likely failed for that service"
        );
    }
}

#[test]
fn aws_profile_option_has_native_transform_generator() {
    // --profile is the only AWS generator the converter could lower from
    // upstream postProcess to a native transform pipeline; the others land
    // as requires_js. If --profile ever flips back to requires_js or loses
    // its split_lines+filter_empty+trim transforms, profile completion
    // stops working — surface that loudly.
    let aws = load_aws_spec();
    let profile = aws
        .options
        .iter()
        .find(|o| o.name.iter().any(|n| n == "--profile"))
        .expect("aws.options must contain --profile (set in upstream src/aws.ts)");

    let args = profile
        .args
        .as_ref()
        .expect("--profile must have args (the profile-name positional)");
    let gen = args
        .generators
        .first()
        .expect("--profile must have at least one generator");

    assert!(
        !gen.requires_js,
        "--profile generator must NOT be requires_js — the transform \
         pipeline should have lowered it. If it flipped back, profile \
         completion has regressed."
    );
    assert!(
        gen.script
            .as_ref()
            .map(|v| v.iter().any(|s| s == "list-profiles"))
            .unwrap_or(false),
        "--profile generator must invoke `aws configure list-profiles`; \
         got script={:?}",
        gen.script
    );
    let transform_names: Vec<&str> = gen
        .transforms
        .iter()
        .map(gc_suggest::transform::transform_name)
        .collect();
    for must in ["split_lines", "filter_empty"] {
        assert!(
            transform_names.contains(&must),
            "--profile generator must include `{must}` transform; got {transform_names:?}"
        );
    }
}

#[test]
fn aws_s3_has_core_action_subcommands() {
    // The s3 service-spec in upstream lists 9 actions: cp, ls, mb, mv,
    // presign, rb, rm, sync, website. Order is converter-determined; we
    // assert presence by name.
    let aws = load_aws_spec();
    let s3 = aws
        .subcommands
        .iter()
        .find(|s| s.name == "s3")
        .expect("aws.subcommands must contain s3");
    let actions: std::collections::HashSet<&str> =
        s3.subcommands.iter().map(|a| a.name.as_str()).collect();
    for must_have in [
        "cp", "ls", "mb", "mv", "presign", "rb", "rm", "sync", "website",
    ] {
        assert!(
            actions.contains(must_have),
            "aws s3 is missing action `{must_have}`; got {actions:?}"
        );
    }
}

#[test]
fn aws_ec2_describe_instances_has_options() {
    // ec2 describe-instances is one of the most-used AWS commands; if the
    // loadSpec resolver dropped its options the static flag UX is broken.
    let aws = load_aws_spec();
    let ec2 = aws
        .subcommands
        .iter()
        .find(|s| s.name == "ec2")
        .expect("aws.subcommands must contain ec2");
    let describe = ec2
        .subcommands
        .iter()
        .find(|s| s.name == "describe-instances")
        .expect("aws ec2 must contain describe-instances");
    assert!(
        describe.options.len() > 5,
        "aws ec2 describe-instances should have many options; got {}",
        describe.options.len()
    );
}

#[test]
fn aws_no_corrected_in_warnings() {
    // The `_corrected_in` field is set on generators that were silently
    // mis-converted in a previous release and corrected later; doctor
    // warns the user about them. The AWS spec is a fresh add in ux-8,
    // so no generator should carry _corrected_in. If one appears, either
    // the converter is mistakenly applying a correction marker or someone
    // copy-pasted from a corrected spec.
    let aws = load_aws_spec();
    let mut corrected: Vec<String> = Vec::new();
    fn walk(sub: &gc_suggest::specs::SubcommandSpec, path: String, out: &mut Vec<String>) {
        for opt in &sub.options {
            if let Some(args) = opt.args.as_ref() {
                for g in &args.generators {
                    if g.corrected_in.is_some() {
                        out.push(format!("{path} {}", opt.name.join("/")));
                    }
                }
            }
        }
        for arg in &sub.args {
            for g in &arg.generators {
                if g.corrected_in.is_some() {
                    out.push(format!("{path} (positional)"));
                }
            }
        }
        for child in &sub.subcommands {
            walk(child, format!("{path} {}", child.name), out);
        }
    }
    let aws_root = gc_suggest::specs::SubcommandSpec {
        name: "aws".to_string(),
        description: aws.description.clone(),
        subcommands: aws.subcommands.clone(),
        options: aws.options.clone(),
        args: aws.args.clone(),
        priority: None,
    };
    walk(&aws_root, "aws".to_string(), &mut corrected);
    assert!(
        corrected.is_empty(),
        "aws is a fresh add and must not carry _corrected_in markers; got: {corrected:?}"
    );
}

#[test]
fn aws_top_level_suggests_service_subcommands() {
    // End-to-end: typing `aws s3` should fuzzy-match the s3 service. Using
    // `aws ` with no query would land alphabetically and `s3` falls outside
    // the default 50-entry result window — the popup is paginated, so the
    // realistic test is "user types enough to narrow the search".
    let engine = build_engine();
    let buffer = "aws s3";
    let ctx = ctx_from(buffer);
    let result = engine.suggest_sync(&ctx, &tmp_cwd(), buffer).unwrap();
    let names: Vec<&str> = result
        .suggestions
        .iter()
        .filter(|s| s.kind == SuggestionKind::Subcommand)
        .map(|s| s.text.as_str())
        .collect();
    assert!(
        names.contains(&"s3"),
        "aws s3 must surface the s3 service; got {names:?}"
    );
    assert!(
        !result
            .suggestions
            .iter()
            .any(|s| matches!(s.kind, SuggestionKind::FilePath | SuggestionKind::Directory)),
        "aws s3 must not leak filesystem; got {:?}",
        result
            .suggestions
            .iter()
            .map(|s| (&s.text, s.kind))
            .collect::<Vec<_>>()
    );
}

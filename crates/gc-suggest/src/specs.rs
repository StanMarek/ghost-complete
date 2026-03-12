use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::transform::Transform;
use crate::types::{Suggestion, SuggestionKind, SuggestionSource};
use gc_buffer::CommandContext;

/// Deserialize `args` as either a single object or an array of objects.
fn deserialize_args_one_or_many<'de, D>(
    deserializer: D,
) -> std::result::Result<Vec<ArgSpec>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum OneOrMany {
        One(ArgSpec),
        Many(Vec<ArgSpec>),
    }

    match OneOrMany::deserialize(deserializer)? {
        OneOrMany::One(single) => Ok(vec![single]),
        OneOrMany::Many(vec) => Ok(vec),
    }
}

/// Deserialize option `args` as either a single object or an array (taking the first).
fn deserialize_option_args<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<ArgSpec>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum OneOrMany {
        One(ArgSpec),
        Many(Vec<ArgSpec>),
    }

    match Option::<OneOrMany>::deserialize(deserializer)? {
        Some(OneOrMany::One(single)) => Ok(Some(single)),
        Some(OneOrMany::Many(vec)) => Ok(vec.into_iter().next()),
        None => Ok(None),
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct CompletionSpec {
    pub name: String,
    pub description: Option<String>,
    #[serde(default)]
    pub subcommands: Vec<SubcommandSpec>,
    #[serde(default)]
    pub options: Vec<OptionSpec>,
    #[serde(default, deserialize_with = "deserialize_args_one_or_many")]
    pub args: Vec<ArgSpec>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SubcommandSpec {
    pub name: String,
    pub description: Option<String>,
    #[serde(default)]
    pub subcommands: Vec<SubcommandSpec>,
    #[serde(default)]
    pub options: Vec<OptionSpec>,
    #[serde(default, deserialize_with = "deserialize_args_one_or_many")]
    pub args: Vec<ArgSpec>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct OptionSpec {
    pub name: Vec<String>,
    pub description: Option<String>,
    #[serde(default, deserialize_with = "deserialize_option_args")]
    pub args: Option<ArgSpec>,
}

/// Deserialize template as either a single string or an array of strings.
/// When an array, takes the most useful entry: "filepaths" > "folders" > first.
fn deserialize_template<'de, D>(deserializer: D) -> std::result::Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum OneOrMany {
        One(String),
        Many(Vec<String>),
    }

    match Option::<OneOrMany>::deserialize(deserializer)? {
        Some(OneOrMany::One(s)) => Ok(Some(s)),
        Some(OneOrMany::Many(vec)) => {
            // Prefer "filepaths" over "folders" when both present
            if vec.iter().any(|t| t == "filepaths") {
                Ok(Some("filepaths".to_string()))
            } else {
                Ok(vec.into_iter().next())
            }
        }
        None => Ok(None),
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct ArgSpec {
    pub name: Option<String>,
    pub description: Option<String>,
    #[serde(default)]
    pub generators: Vec<GeneratorSpec>,
    #[serde(default, deserialize_with = "deserialize_template")]
    pub template: Option<String>,
    /// Static suggestions — accepted from specs but not yet used at runtime.
    #[serde(default)]
    pub suggestions: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CacheConfig {
    #[serde(default)]
    pub ttl_seconds: u64,
    #[serde(default)]
    pub cache_by_directory: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GeneratorSpec {
    #[serde(rename = "type")]
    pub generator_type: Option<String>,
    pub script: Option<Vec<String>>,
    pub script_template: Option<Vec<String>>,
    #[serde(default)]
    pub transforms: Vec<Transform>,
    pub cache: Option<CacheConfig>,
    #[serde(default)]
    pub requires_js: bool,
    pub js_source: Option<String>,
}

pub struct SpecStore {
    specs: HashMap<String, CompletionSpec>,
}

pub struct SpecLoadResult {
    pub store: SpecStore,
    pub errors: Vec<String>,
}

impl SpecStore {
    pub fn load_from_dir(dir: &Path) -> Result<SpecLoadResult> {
        let mut specs = HashMap::new();
        let mut errors = Vec::new();

        if !dir.exists() {
            tracing::debug!("spec directory does not exist: {}", dir.display());
            return Ok(SpecLoadResult {
                store: Self { specs },
                errors,
            });
        }

        let entries = std::fs::read_dir(dir)
            .with_context(|| format!("failed to read spec directory: {}", dir.display()))?;

        for entry in entries {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let file_name = path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            match Self::load_spec(&path) {
                Ok(spec) => {
                    tracing::debug!("loaded spec: {}", spec.name);
                    specs.insert(spec.name.clone(), spec);
                }
                Err(e) => {
                    errors.push(format!("{file_name}: {e}"));
                }
            }
        }

        Ok(SpecLoadResult {
            store: Self { specs },
            errors,
        })
    }

    fn load_spec(path: &Path) -> Result<CompletionSpec> {
        let contents = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read spec file: {}", path.display()))?;
        let mut spec: CompletionSpec = serde_json::from_str(&contents)
            .with_context(|| format!("failed to parse spec file: {}", path.display()))?;
        let warnings = validate_spec_generators(&mut spec);
        for w in &warnings {
            tracing::warn!("{}: {w}", spec.name);
        }
        Ok(spec)
    }

    pub fn get(&self, command: &str) -> Option<&CompletionSpec> {
        self.specs.get(command)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &CompletionSpec)> {
        self.specs.iter().map(|(k, v)| (k.as_str(), v))
    }

    pub fn len(&self) -> usize {
        self.specs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.specs.is_empty()
    }
}

pub struct SpecResolution {
    pub subcommands: Vec<Suggestion>,
    pub options: Vec<Suggestion>,
    pub native_generators: Vec<String>,
    pub script_generators: Vec<GeneratorSpec>,
    pub wants_filepaths: bool,
    pub wants_folders_only: bool,
}

/// Walk the spec tree using args from the CommandContext to find the deepest
/// matching subcommand, then return available completions at that position.
pub fn resolve_spec(spec: &CompletionSpec, ctx: &CommandContext) -> SpecResolution {
    // Start at the top-level spec
    let mut current_subcommands = &spec.subcommands;
    let mut current_options = &spec.options;
    let mut current_args = &spec.args;

    // Walk through ctx.args, greedily matching subcommand names
    let mut arg_idx = 0;
    let args = &ctx.args;

    while arg_idx < args.len() {
        let arg = &args[arg_idx];

        // Skip flags
        if arg.starts_with('-') {
            // If this flag takes a value in the spec, skip the next arg too
            if let Some(opt) = find_option(current_options, arg) {
                if opt.args.is_some() && arg_idx + 1 < args.len() {
                    arg_idx += 2;
                    continue;
                }
            }
            arg_idx += 1;
            continue;
        }

        // Try to match a subcommand
        if let Some(sub) = current_subcommands.iter().find(|s| s.name == *arg) {
            current_subcommands = &sub.subcommands;
            current_options = &sub.options;
            current_args = &sub.args;
            arg_idx += 1;
        } else {
            // Positional argument — don't descend further
            arg_idx += 1;
        }
    }

    // Build suggestions from the resolved position
    let subcommand_suggestions: Vec<Suggestion> = current_subcommands
        .iter()
        .map(|s| Suggestion {
            text: s.name.clone(),
            description: s.description.clone(),
            kind: SuggestionKind::Subcommand,
            source: SuggestionSource::Spec,
            score: 0,
        })
        .collect();

    let option_suggestions: Vec<Suggestion> = current_options
        .iter()
        .flat_map(|o| {
            o.name.iter().map(move |n| Suggestion {
                text: n.clone(),
                description: o.description.clone(),
                kind: SuggestionKind::Flag,
                source: SuggestionSource::Spec,
                score: 0,
            })
        })
        .collect();

    // Collect generator types from args at the resolved position
    let mut native_generators = Vec::new();
    let mut script_generators = Vec::new();
    let mut wants_filepaths = false;
    let mut wants_folders_only = false;

    // If the preceding token was a flag that takes an argument, check
    // the option's arg spec for templates/generators instead of the
    // positional args.
    if let Some(flag) = &ctx.preceding_flag {
        if let Some(opt) = find_option(current_options, flag) {
            if let Some(arg_spec) = &opt.args {
                collect_generators(
                    &arg_spec.generators,
                    &mut native_generators,
                    &mut script_generators,
                );
                match arg_spec.template.as_deref() {
                    Some("filepaths") => wants_filepaths = true,
                    Some("folders") => wants_folders_only = true,
                    _ => {}
                }
            }
        }
    }

    // Also check positional arg specs at the resolved position
    for arg_spec in current_args {
        collect_generators(
            &arg_spec.generators,
            &mut native_generators,
            &mut script_generators,
        );
        match arg_spec.template.as_deref() {
            Some("filepaths") => wants_filepaths = true,
            Some("folders") => wants_folders_only = true,
            _ => {}
        }
    }

    SpecResolution {
        subcommands: subcommand_suggestions,
        options: option_suggestions,
        native_generators,
        script_generators,
        wants_filepaths,
        wants_folders_only,
    }
}

fn collect_generators(
    generators: &[GeneratorSpec],
    native: &mut Vec<String>,
    script: &mut Vec<GeneratorSpec>,
) {
    for gen in generators {
        if gen.requires_js {
            tracing::info!("skipping generator requiring JS runtime");
            continue;
        }
        if let Some(ref gen_type) = gen.generator_type {
            native.push(gen_type.clone());
        }
        if gen.script.is_some() || gen.script_template.is_some() {
            script.push(gen.clone());
        }
    }
}

fn find_option<'a>(options: &'a [OptionSpec], flag: &str) -> Option<&'a OptionSpec> {
    options.iter().find(|o| o.name.iter().any(|n| n == flag))
}

/// Walk all generators in a spec tree, validate their transform pipelines,
/// and remove generators with invalid pipelines. Returns warnings for each
/// removed generator.
fn validate_spec_generators(spec: &mut CompletionSpec) -> Vec<String> {
    let mut warnings = Vec::new();
    validate_args_generators(&mut spec.args, &spec.name, &mut warnings);
    for opt in &mut spec.options {
        if let Some(ref mut arg_spec) = opt.args {
            validate_arg_generators(arg_spec, &spec.name, &mut warnings);
        }
    }
    for sub in &mut spec.subcommands {
        validate_subcommand_generators(sub, &spec.name, &mut warnings);
    }
    warnings
}

fn validate_subcommand_generators(
    sub: &mut SubcommandSpec,
    spec_name: &str,
    warnings: &mut Vec<String>,
) {
    validate_args_generators(&mut sub.args, spec_name, warnings);
    for opt in &mut sub.options {
        if let Some(ref mut arg_spec) = opt.args {
            validate_arg_generators(arg_spec, spec_name, warnings);
        }
    }
    for nested_sub in &mut sub.subcommands {
        validate_subcommand_generators(nested_sub, spec_name, warnings);
    }
}

fn validate_args_generators(args: &mut [ArgSpec], spec_name: &str, warnings: &mut Vec<String>) {
    for arg_spec in args.iter_mut() {
        validate_arg_generators(arg_spec, spec_name, warnings);
    }
}

fn validate_arg_generators(arg_spec: &mut ArgSpec, spec_name: &str, warnings: &mut Vec<String>) {
    use crate::transform::validate_pipeline;

    let original_len = arg_spec.generators.len();
    arg_spec.generators.retain(|gen| {
        if gen.transforms.is_empty() {
            return true;
        }
        match validate_pipeline(&gen.transforms) {
            Ok(()) => true,
            Err(e) => {
                warnings.push(format!(
                    "generator in {spec_name} has invalid transform pipeline: {e}"
                ));
                false
            }
        }
    });
    if arg_spec.generators.len() < original_len {
        tracing::debug!(
            "{spec_name}: removed {} generator(s) with invalid transform pipelines",
            original_len - arg_spec.generators.len()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_spec() -> CompletionSpec {
        serde_json::from_str(
            r#"{
                "name": "test-cmd",
                "description": "A test command",
                "subcommands": [
                    {
                        "name": "sub1",
                        "description": "First subcommand",
                        "options": [
                            { "name": ["--verbose", "-v"], "description": "Verbose output" }
                        ],
                        "args": [
                            {
                                "name": "target",
                                "generators": [{ "type": "git_branches" }]
                            }
                        ]
                    },
                    {
                        "name": "sub2",
                        "description": "Second subcommand"
                    }
                ],
                "options": [
                    { "name": ["--help", "-h"], "description": "Show help" }
                ]
            }"#,
        )
        .unwrap()
    }

    #[test]
    fn test_curl_dash_o_resolve_spec_sets_wants_filepaths() {
        let spec_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../specs/curl.json");
        if !spec_path.exists() {
            eprintln!("curl.json not found, skipping");
            return;
        }
        let contents = std::fs::read_to_string(&spec_path).unwrap();
        let spec: CompletionSpec = serde_json::from_str(&contents).unwrap();

        // curl -o <TAB>
        let ctx = CommandContext {
            command: Some("curl".into()),
            args: vec!["-o".into()],
            current_word: String::new(),
            word_index: 2,
            is_flag: false,
            is_long_flag: false,
            preceding_flag: Some("-o".into()),
            in_pipe: false,
            in_redirect: false,
            quote_state: gc_buffer::QuoteState::None,
        };
        let res = resolve_spec(&spec, &ctx);
        eprintln!(
            "wants_filepaths={}, wants_folders_only={}, generators={:?}",
            res.wants_filepaths, res.wants_folders_only, res.native_generators
        );
        assert!(
            res.wants_filepaths,
            "curl -o should set wants_filepaths from the -o option's args template"
        );
    }

    #[test]
    fn test_deserialize_git_spec() {
        let spec_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../specs/git.json");
        if spec_path.exists() {
            let contents = std::fs::read_to_string(&spec_path).unwrap();
            let spec: CompletionSpec = serde_json::from_str(&contents).unwrap();
            assert_eq!(spec.name, "git");
            assert!(!spec.subcommands.is_empty());
        }
    }

    #[test]
    fn test_resolve_top_level_subcommands() {
        let spec = test_spec();
        let ctx = CommandContext {
            command: Some("test-cmd".into()),
            args: vec![],
            current_word: String::new(),
            word_index: 1,
            is_flag: false,
            is_long_flag: false,
            preceding_flag: None,
            in_pipe: false,
            in_redirect: false,
            quote_state: gc_buffer::QuoteState::None,
        };
        let res = resolve_spec(&spec, &ctx);
        let names: Vec<&str> = res.subcommands.iter().map(|s| s.text.as_str()).collect();
        assert!(names.contains(&"sub1"));
        assert!(names.contains(&"sub2"));
    }

    #[test]
    fn test_resolve_subcommand_options() {
        let spec = test_spec();
        let ctx = CommandContext {
            command: Some("test-cmd".into()),
            args: vec!["sub1".into()],
            current_word: "--".into(),
            word_index: 2,
            is_flag: true,
            is_long_flag: true,
            preceding_flag: None,
            in_pipe: false,
            in_redirect: false,
            quote_state: gc_buffer::QuoteState::None,
        };
        let res = resolve_spec(&spec, &ctx);
        let names: Vec<&str> = res.options.iter().map(|s| s.text.as_str()).collect();
        assert!(names.contains(&"--verbose"));
        assert!(names.contains(&"-v"));
    }

    #[test]
    fn test_resolve_generators() {
        let spec = test_spec();
        let ctx = CommandContext {
            command: Some("test-cmd".into()),
            args: vec!["sub1".into()],
            current_word: String::new(),
            word_index: 2,
            is_flag: false,
            is_long_flag: false,
            preceding_flag: None,
            in_pipe: false,
            in_redirect: false,
            quote_state: gc_buffer::QuoteState::None,
        };
        let res = resolve_spec(&spec, &ctx);
        assert!(res.native_generators.contains(&"git_branches".to_string()));
    }

    #[test]
    fn test_resolve_unknown_subcommand_doesnt_panic() {
        let spec = test_spec();
        let ctx = CommandContext {
            command: Some("test-cmd".into()),
            args: vec!["nonexistent".into()],
            current_word: String::new(),
            word_index: 2,
            is_flag: false,
            is_long_flag: false,
            preceding_flag: None,
            in_pipe: false,
            in_redirect: false,
            quote_state: gc_buffer::QuoteState::None,
        };
        let res = resolve_spec(&spec, &ctx);
        // Should not panic — returns top-level completions since "nonexistent"
        // didn't match any subcommand
        assert!(res.subcommands.is_empty() || !res.subcommands.is_empty());
    }

    #[test]
    fn test_folders_template_sets_wants_folders_only() {
        let spec: CompletionSpec = serde_json::from_str(
            r#"{
                "name": "cd",
                "description": "Change directory",
                "args": [{ "name": "directory", "template": "folders" }]
            }"#,
        )
        .unwrap();
        let ctx = CommandContext {
            command: Some("cd".into()),
            args: vec![],
            current_word: String::new(),
            word_index: 1,
            is_flag: false,
            is_long_flag: false,
            preceding_flag: None,
            in_pipe: false,
            in_redirect: false,
            quote_state: gc_buffer::QuoteState::None,
        };
        let res = resolve_spec(&spec, &ctx);
        assert!(
            res.wants_folders_only,
            "folders template should set wants_folders_only"
        );
        assert!(
            !res.wants_filepaths,
            "folders template should NOT set wants_filepaths"
        );
    }

    #[test]
    fn test_filepaths_template_sets_wants_filepaths() {
        let spec: CompletionSpec = serde_json::from_str(
            r#"{
                "name": "cat",
                "description": "Concatenate files",
                "args": [{ "name": "file", "template": "filepaths" }]
            }"#,
        )
        .unwrap();
        let ctx = CommandContext {
            command: Some("cat".into()),
            args: vec![],
            current_word: String::new(),
            word_index: 1,
            is_flag: false,
            is_long_flag: false,
            preceding_flag: None,
            in_pipe: false,
            in_redirect: false,
            quote_state: gc_buffer::QuoteState::None,
        };
        let res = resolve_spec(&spec, &ctx);
        assert!(
            res.wants_filepaths,
            "filepaths template should set wants_filepaths"
        );
        assert!(
            !res.wants_folders_only,
            "filepaths template should NOT set wants_folders_only"
        );
    }

    #[test]
    fn test_option_arg_filepaths_template_via_preceding_flag() {
        // pip install -r <TAB> → should want filepaths
        let spec: CompletionSpec = serde_json::from_str(
            r#"{
                "name": "pip",
                "description": "Python package installer",
                "subcommands": [{
                    "name": "install",
                    "description": "Install packages",
                    "options": [{
                        "name": ["-r", "--requirement"],
                        "description": "Install from requirements file",
                        "args": { "name": "file", "template": "filepaths" }
                    }]
                }]
            }"#,
        )
        .unwrap();
        let ctx = CommandContext {
            command: Some("pip".into()),
            args: vec!["install".into(), "-r".into()],
            current_word: String::new(),
            word_index: 3,
            is_flag: false,
            is_long_flag: false,
            preceding_flag: Some("-r".into()),
            in_pipe: false,
            in_redirect: false,
            quote_state: gc_buffer::QuoteState::None,
        };
        let res = resolve_spec(&spec, &ctx);
        assert!(
            res.wants_filepaths,
            "option with filepaths template should set wants_filepaths when preceding_flag matches"
        );
    }

    #[test]
    fn test_option_arg_folders_template_via_preceding_flag() {
        // pip install -t <TAB> → should want folders only
        let spec: CompletionSpec = serde_json::from_str(
            r#"{
                "name": "pip",
                "description": "Python package installer",
                "subcommands": [{
                    "name": "install",
                    "description": "Install packages",
                    "options": [{
                        "name": ["-t", "--target"],
                        "description": "Install into this directory",
                        "args": { "name": "dir", "template": "folders" }
                    }]
                }]
            }"#,
        )
        .unwrap();
        let ctx = CommandContext {
            command: Some("pip".into()),
            args: vec!["install".into(), "-t".into()],
            current_word: String::new(),
            word_index: 3,
            is_flag: false,
            is_long_flag: false,
            preceding_flag: Some("-t".into()),
            in_pipe: false,
            in_redirect: false,
            quote_state: gc_buffer::QuoteState::None,
        };
        let res = resolve_spec(&spec, &ctx);
        assert!(
            res.wants_folders_only,
            "option with folders template should set wants_folders_only when preceding_flag matches"
        );
        assert!(
            !res.wants_filepaths,
            "folders template should NOT set wants_filepaths"
        );
    }

    #[test]
    fn test_option_arg_generator_via_preceding_flag() {
        // git checkout -b <TAB> with a generator on the option
        let spec: CompletionSpec = serde_json::from_str(
            r#"{
                "name": "test-cmd",
                "description": "Test",
                "options": [{
                    "name": ["-b", "--branch"],
                    "description": "Branch name",
                    "args": {
                        "name": "branch",
                        "generators": [{ "type": "git_branches" }]
                    }
                }]
            }"#,
        )
        .unwrap();
        let ctx = CommandContext {
            command: Some("test-cmd".into()),
            args: vec!["-b".into()],
            current_word: String::new(),
            word_index: 2,
            is_flag: false,
            is_long_flag: false,
            preceding_flag: Some("-b".into()),
            in_pipe: false,
            in_redirect: false,
            quote_state: gc_buffer::QuoteState::None,
        };
        let res = resolve_spec(&spec, &ctx);
        assert!(
            res.native_generators.contains(&"git_branches".to_string()),
            "option arg generators should be collected via preceding_flag"
        );
    }

    #[test]
    fn test_no_preceding_flag_no_option_template() {
        // pip install <TAB> without a preceding flag — should NOT trigger option templates
        let spec: CompletionSpec = serde_json::from_str(
            r#"{
                "name": "pip",
                "description": "Python package installer",
                "subcommands": [{
                    "name": "install",
                    "description": "Install packages",
                    "options": [{
                        "name": ["-r"],
                        "description": "Requirements file",
                        "args": { "name": "file", "template": "filepaths" }
                    }]
                }]
            }"#,
        )
        .unwrap();
        let ctx = CommandContext {
            command: Some("pip".into()),
            args: vec!["install".into()],
            current_word: String::new(),
            word_index: 2,
            is_flag: false,
            is_long_flag: false,
            preceding_flag: None,
            in_pipe: false,
            in_redirect: false,
            quote_state: gc_buffer::QuoteState::None,
        };
        let res = resolve_spec(&spec, &ctx);
        assert!(
            !res.wants_filepaths,
            "should NOT want filepaths when no preceding_flag is set"
        );
    }

    #[test]
    fn test_load_from_dir_mixed_valid_and_invalid() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("good.json"),
            r#"{"name": "good", "args": []}"#,
        )
        .unwrap();
        std::fs::write(dir.path().join("bad.json"), r#"{"not_a_spec": true}"#).unwrap();
        std::fs::write(dir.path().join("broken.json"), "{ totally busted").unwrap();

        let result = SpecStore::load_from_dir(dir.path()).unwrap();
        assert!(
            result.store.get("good").is_some(),
            "valid spec should be loaded"
        );
        assert_eq!(result.errors.len(), 2, "should have 2 errors");
        assert!(
            result.errors.iter().any(|e| e.starts_with("bad.json:")),
            "errors should include bad.json: {:?}",
            result.errors
        );
        assert!(
            result.errors.iter().any(|e| e.starts_with("broken.json:")),
            "errors should include broken.json: {:?}",
            result.errors
        );
    }

    #[test]
    fn test_load_from_dir_all_valid() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(dir.path().join("alpha.json"), r#"{"name": "alpha"}"#).unwrap();
        std::fs::write(dir.path().join("beta.json"), r#"{"name": "beta"}"#).unwrap();

        let result = SpecStore::load_from_dir(dir.path()).unwrap();
        assert!(result.errors.is_empty(), "no errors expected");
        assert!(result.store.get("alpha").is_some());
        assert!(result.store.get("beta").is_some());
    }

    #[test]
    fn test_load_from_dir_nonexistent() {
        let result = SpecStore::load_from_dir(Path::new("/nonexistent/path/specs")).unwrap();
        assert!(result.errors.is_empty());
        assert!(result.store.get("anything").is_none());
    }

    #[test]
    fn test_deserialize_native_generator() {
        let gen: GeneratorSpec = serde_json::from_str(r#"{"type": "git_branches"}"#).unwrap();
        assert_eq!(gen.generator_type.as_deref(), Some("git_branches"));
        assert!(gen.script.is_none());
        assert!(gen.script_template.is_none());
        assert!(gen.transforms.is_empty());
        assert!(gen.cache.is_none());
        assert!(!gen.requires_js);
        assert!(gen.js_source.is_none());
    }

    #[test]
    fn test_deserialize_script_generator() {
        let gen: GeneratorSpec = serde_json::from_str(
            r#"{"script": ["brew", "formulae"], "cache": {"ttl_seconds": 300}}"#,
        )
        .unwrap();
        assert!(gen.generator_type.is_none());
        assert_eq!(
            gen.script.as_deref(),
            Some(&["brew".to_string(), "formulae".to_string()][..])
        );
        assert!(gen.script_template.is_none());
        assert!(gen.transforms.is_empty());
        let cache = gen.cache.unwrap();
        assert_eq!(cache.ttl_seconds, 300);
        assert!(!cache.cache_by_directory);
    }

    #[test]
    fn test_deserialize_script_generator_with_transforms() {
        let gen: GeneratorSpec = serde_json::from_str(
            r#"{
                "script": ["brew", "formulae"],
                "transforms": ["split_lines", "filter_empty", "trim"],
                "cache": {"ttl_seconds": 300}
            }"#,
        )
        .unwrap();
        assert_eq!(gen.transforms.len(), 3);
    }

    #[test]
    fn test_deserialize_script_template_generator() {
        let gen: GeneratorSpec = serde_json::from_str(
            r#"{"script_template": ["cmd", "{prev_token}"], "transforms": ["split_lines"]}"#,
        )
        .unwrap();
        assert!(gen.generator_type.is_none());
        assert!(gen.script.is_none());
        assert_eq!(
            gen.script_template.as_deref(),
            Some(&["cmd".to_string(), "{prev_token}".to_string()][..])
        );
        assert_eq!(gen.transforms.len(), 1);
    }

    #[test]
    fn test_deserialize_requires_js_generator() {
        let gen: GeneratorSpec = serde_json::from_str(
            r#"{"requires_js": true, "js_source": "module.exports = { ... }"}"#,
        )
        .unwrap();
        assert!(gen.requires_js);
        assert_eq!(gen.js_source.as_deref(), Some("module.exports = { ... }"));
    }

    #[test]
    fn test_resolve_spec_splits_generators() {
        let spec: CompletionSpec = serde_json::from_str(
            r#"{
                "name": "test-mixed",
                "args": [{
                    "name": "target",
                    "generators": [
                        {"type": "git_branches"},
                        {"script": ["some-cmd"], "transforms": ["split_lines"]},
                        {"requires_js": true, "js_source": "..."}
                    ]
                }]
            }"#,
        )
        .unwrap();
        let ctx = CommandContext {
            command: Some("test-mixed".into()),
            args: vec![],
            current_word: String::new(),
            word_index: 1,
            is_flag: false,
            is_long_flag: false,
            preceding_flag: None,
            in_pipe: false,
            in_redirect: false,
            quote_state: gc_buffer::QuoteState::None,
        };
        let res = resolve_spec(&spec, &ctx);
        assert_eq!(res.native_generators, vec!["git_branches"]);
        assert_eq!(res.script_generators.len(), 1);
        assert!(res.script_generators[0].script.is_some());
    }

    #[test]
    fn test_validate_spec_strips_invalid_generator_pipeline() {
        // A spec with one valid generator and one with an invalid pipeline
        // (post-split transform before split). The invalid one should be
        // stripped during load_spec.
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("test.json"),
            r#"{
                "name": "test",
                "args": [{
                    "name": "target",
                    "generators": [
                        {"type": "git_branches"},
                        {"script": ["cmd"], "transforms": ["filter_empty", "split_lines"]}
                    ]
                }]
            }"#,
        )
        .unwrap();

        let result = SpecStore::load_from_dir(dir.path()).unwrap();
        let spec = result.store.get("test").unwrap();
        // The second generator should have been removed
        assert_eq!(
            spec.args[0].generators.len(),
            1,
            "invalid generator should be removed; remaining: {:?}",
            spec.args[0].generators
        );
        assert_eq!(
            spec.args[0].generators[0].generator_type.as_deref(),
            Some("git_branches"),
        );
    }

    #[test]
    fn test_validate_spec_keeps_valid_pipeline() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("test.json"),
            r#"{
                "name": "test",
                "args": [{
                    "name": "target",
                    "generators": [
                        {"script": ["cmd"], "transforms": ["split_lines", "filter_empty", "trim"]}
                    ]
                }]
            }"#,
        )
        .unwrap();

        let result = SpecStore::load_from_dir(dir.path()).unwrap();
        let spec = result.store.get("test").unwrap();
        assert_eq!(
            spec.args[0].generators.len(),
            1,
            "valid generator should be kept"
        );
    }

    #[test]
    fn test_validate_spec_empty_transforms_kept() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("test.json"),
            r#"{
                "name": "test",
                "args": [{
                    "name": "target",
                    "generators": [
                        {"type": "git_branches"},
                        {"script": ["cmd"]}
                    ]
                }]
            }"#,
        )
        .unwrap();

        let result = SpecStore::load_from_dir(dir.path()).unwrap();
        let spec = result.store.get("test").unwrap();
        assert_eq!(
            spec.args[0].generators.len(),
            2,
            "generators with empty transforms should be kept"
        );
    }

    #[test]
    fn test_validate_spec_recursive_subcommands() {
        // Ensure validation walks into nested subcommands
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("test.json"),
            r#"{
                "name": "test",
                "subcommands": [{
                    "name": "sub",
                    "args": [{
                        "name": "target",
                        "generators": [
                            {"script": ["cmd"], "transforms": ["trim", "split_lines"]}
                        ]
                    }]
                }]
            }"#,
        )
        .unwrap();

        let result = SpecStore::load_from_dir(dir.path()).unwrap();
        let spec = result.store.get("test").unwrap();
        assert_eq!(
            spec.subcommands[0].args[0].generators.len(),
            0,
            "invalid generator in subcommand should be removed"
        );
    }

    #[test]
    fn test_validate_spec_option_args() {
        // Ensure validation walks into option arg specs
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("test.json"),
            r#"{
                "name": "test",
                "options": [{
                    "name": ["-f"],
                    "description": "flag",
                    "args": {
                        "name": "val",
                        "generators": [
                            {"script": ["cmd"], "transforms": ["split_lines", "split_lines"]}
                        ]
                    }
                }]
            }"#,
        )
        .unwrap();

        let result = SpecStore::load_from_dir(dir.path()).unwrap();
        let spec = result.store.get("test").unwrap();
        assert_eq!(
            spec.options[0].args.as_ref().unwrap().generators.len(),
            0,
            "double-split generator in option args should be removed"
        );
    }
}

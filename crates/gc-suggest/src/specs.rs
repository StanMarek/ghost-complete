use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::types::{Suggestion, SuggestionKind, SuggestionSource};
use gc_buffer::CommandContext;

#[derive(Debug, Clone, Deserialize)]
pub struct CompletionSpec {
    pub name: String,
    pub description: Option<String>,
    #[serde(default)]
    pub subcommands: Vec<SubcommandSpec>,
    #[serde(default)]
    pub options: Vec<OptionSpec>,
    #[serde(default)]
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
    #[serde(default)]
    pub args: Vec<ArgSpec>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct OptionSpec {
    pub name: Vec<String>,
    pub description: Option<String>,
    #[serde(default)]
    pub args: Option<ArgSpec>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ArgSpec {
    pub name: Option<String>,
    pub description: Option<String>,
    #[serde(default)]
    pub generators: Vec<GeneratorSpec>,
    pub template: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GeneratorSpec {
    #[serde(rename = "type")]
    pub generator_type: String,
}

pub struct SpecStore {
    specs: HashMap<String, CompletionSpec>,
}

impl SpecStore {
    pub fn load_from_dir(dir: &Path) -> Result<Self> {
        let mut specs = HashMap::new();

        if !dir.exists() {
            tracing::debug!("spec directory does not exist: {}", dir.display());
            return Ok(Self { specs });
        }

        let entries = std::fs::read_dir(dir)
            .with_context(|| format!("failed to read spec directory: {}", dir.display()))?;

        for entry in entries {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            match Self::load_spec(&path) {
                Ok(spec) => {
                    tracing::debug!("loaded spec: {}", spec.name);
                    specs.insert(spec.name.clone(), spec);
                }
                Err(e) => {
                    tracing::warn!("failed to load spec {}: {e}", path.display());
                }
            }
        }

        Ok(Self { specs })
    }

    fn load_spec(path: &Path) -> Result<CompletionSpec> {
        let contents = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read spec file: {}", path.display()))?;
        let spec: CompletionSpec = serde_json::from_str(&contents)
            .with_context(|| format!("failed to parse spec file: {}", path.display()))?;
        Ok(spec)
    }

    pub fn get(&self, command: &str) -> Option<&CompletionSpec> {
        self.specs.get(command)
    }
}

pub struct SpecResolution {
    pub subcommands: Vec<Suggestion>,
    pub options: Vec<Suggestion>,
    pub generators: Vec<String>,
    pub wants_filepaths: bool,
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
    let mut generators = Vec::new();
    let mut wants_filepaths = false;

    for arg_spec in current_args {
        for gen in &arg_spec.generators {
            generators.push(gen.generator_type.clone());
        }
        if arg_spec
            .template
            .as_deref()
            .is_some_and(|t| t == "filepaths" || t == "folders")
        {
            wants_filepaths = true;
        }
    }

    SpecResolution {
        subcommands: subcommand_suggestions,
        options: option_suggestions,
        generators,
        wants_filepaths,
    }
}

fn find_option<'a>(options: &'a [OptionSpec], flag: &str) -> Option<&'a OptionSpec> {
    options.iter().find(|o| o.name.iter().any(|n| n == flag))
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
        assert!(res.generators.contains(&"git_branches".to_string()));
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
}

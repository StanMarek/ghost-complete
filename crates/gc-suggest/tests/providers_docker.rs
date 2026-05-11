use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::Arc;

pub mod types {
    pub use gc_suggest::types::*;
}

mod providers {
    pub use gc_suggest::providers::{Provider, ProviderCtx};

    pub mod util {
        include!("../src/providers/util.rs");
    }

    pub mod docker {
        include!("../src/providers/docker.rs");
    }
}

use gc_suggest::providers::{Provider, ProviderCtx};
use gc_suggest::types::{SuggestionKind, SuggestionSource};
use providers::docker::{
    parse_docker_json_lines, DockerContainers, DockerImages, DockerKind, DockerNetworks,
    DockerRunningContainers, DockerVolumes,
};

fn texts(suggestions: &[types::Suggestion]) -> Vec<&str> {
    suggestions.iter().map(|s| s.text.as_str()).collect()
}

fn ctx_with_binary(cwd: &Path, binary: &str) -> ProviderCtx {
    ProviderCtx {
        cwd: cwd.to_path_buf(),
        env: Arc::new(HashMap::new()),
        current_token: String::new(),
        params: Arc::new(BTreeMap::from([("binary".to_string(), binary.to_string())])),
    }
}

#[test]
fn parses_recorded_docker_json_line_outputs() {
    let images = parse_docker_json_lines(
        r#"{"Repository":"alpine","Tag":"3.19","ID":"sha256:111","Size":"7.4MB"}
{"Repository":"<none>","Tag":"<none>","ID":"sha256:222","Size":"12MB"}
"#,
        DockerKind::Images,
    );
    assert_eq!(texts(&images), vec!["alpine:3.19", "sha256:222"]);
    assert_eq!(images[0].description.as_deref(), Some("sha256:111 7.4MB"));

    let containers = parse_docker_json_lines(
        r#"{"ID":"abc123","Image":"nginx:latest","Names":"web","State":"running","Status":"Up 2 minutes"}
{"ID":"def456","Image":"redis:7","Names":"cache","State":"exited","Status":"Exited (0) 1 hour ago"}
"#,
        DockerKind::Containers,
    );
    assert_eq!(texts(&containers), vec!["web", "cache"]);
    assert_eq!(
        containers[0].description.as_deref(),
        Some("nginx:latest (Up 2 minutes)")
    );

    let running = parse_docker_json_lines(
        r#"{"ID":"abc123","Image":"nginx:latest","Names":"web","State":"running","Status":"Up 2 minutes"}
{"ID":"def456","Image":"redis:7","Names":"cache","State":"exited","Status":"Exited (0) 1 hour ago"}
"#,
        DockerKind::RunningContainers,
    );
    assert_eq!(texts(&running), vec!["web"]);

    let networks = parse_docker_json_lines(
        r#"{"ID":"net123","Name":"bridge","Driver":"bridge","Scope":"local"}
{"id":"net456","name":"podman","driver":"bridge","scope":"local"}
"#,
        DockerKind::Networks,
    );
    assert_eq!(texts(&networks), vec!["bridge", "podman"]);
    assert_eq!(
        networks[0].description.as_deref(),
        Some("net123 bridge local")
    );

    let volumes = parse_docker_json_lines(
        r#"{"Name":"db-data","Driver":"local","Mountpoint":"/var/lib/docker/volumes/db-data/_data"}
{"name":"cache-data","driver":"local","mountpoint":"/var/lib/containers/storage/volumes/cache-data/_data"}
"#,
        DockerKind::Volumes,
    );
    assert_eq!(texts(&volumes), vec!["db-data", "cache-data"]);

    for suggestion in images
        .iter()
        .chain(containers.iter())
        .chain(running.iter())
        .chain(networks.iter())
        .chain(volumes.iter())
    {
        assert_eq!(suggestion.kind, SuggestionKind::ProviderValue);
        assert_eq!(suggestion.source, SuggestionSource::Provider);
    }
}

#[test]
fn docker_empty_output_yields_no_suggestions() {
    for kind in [
        DockerKind::Images,
        DockerKind::Containers,
        DockerKind::RunningContainers,
        DockerKind::Networks,
        DockerKind::Volumes,
    ] {
        assert!(parse_docker_json_lines("", kind).is_empty());
        assert!(parse_docker_json_lines("\n\n", kind).is_empty());
    }
}

#[test]
fn docker_malformed_output_yields_no_suggestions() {
    assert!(parse_docker_json_lines("not json\n", DockerKind::Images).is_empty());
    assert!(parse_docker_json_lines("{\"Repository\":\"ok\"}\n{", DockerKind::Images).is_empty());
}

#[tokio::test]
async fn docker_missing_binary_falls_back_to_empty() {
    let tmp = tempfile::TempDir::new().unwrap();
    let ctx = ctx_with_binary(tmp.path(), "/nonexistent/docker-for-provider-test");

    for result in [
        DockerImages.generate(&ctx).await,
        DockerContainers.generate(&ctx).await,
        DockerRunningContainers.generate(&ctx).await,
        DockerNetworks.generate(&ctx).await,
        DockerVolumes.generate(&ctx).await,
    ] {
        assert!(matches!(result, Ok(ref suggestions) if suggestions.is_empty()));
    }
}

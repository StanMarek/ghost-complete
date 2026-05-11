use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub mod types {
    pub use gc_suggest::types::*;
}

mod providers {
    pub use gc_suggest::providers::{Provider, ProviderCtx};

    pub mod util {
        include!("../src/providers/util.rs");
    }

    pub mod version_probe {
        include!("../src/providers/version_probe.rs");
    }

    pub mod kubectl {
        include!("../src/providers/kubectl.rs");
    }
}

use gc_suggest::providers::ProviderCtx;
use gc_suggest::types::{SuggestionKind, SuggestionSource};
use providers::kubectl::{
    parse_kubectl_json_list, parse_kubectl_lines, K8sContexts, K8sKind, K8sNamespaces, K8sNodes,
    K8sPods, K8sResources, K8sServices,
};

fn texts(suggestions: &[types::Suggestion]) -> Vec<&str> {
    suggestions.iter().map(|s| s.text.as_str()).collect()
}

fn ctx(cwd: &Path) -> ProviderCtx {
    ProviderCtx {
        cwd: cwd.to_path_buf(),
        env: Arc::new(HashMap::new()),
        current_token: String::new(),
        params: Arc::new(BTreeMap::new()),
    }
}

fn ctx_with_kubeconfig(cwd: &Path, kubeconfig: PathBuf) -> ProviderCtx {
    ProviderCtx {
        cwd: cwd.to_path_buf(),
        env: Arc::new(HashMap::from([(
            "KUBECONFIG".to_string(),
            kubeconfig.display().to_string(),
        )])),
        current_token: String::new(),
        params: Arc::new(BTreeMap::new()),
    }
}

#[test]
fn parses_recorded_kubectl_json_outputs() {
    let pods = parse_kubectl_json_list(
        r#"{
  "items": [
    {"metadata": {"name": "api-7f9d", "namespace": "default"}, "status": {"phase": "Running"}},
    {"metadata": {"name": "worker-0", "namespace": "jobs"}, "status": {"phase": "Pending"}}
  ]
}"#,
        K8sKind::Pods,
    );
    assert_eq!(texts(&pods), vec!["api-7f9d", "worker-0"]);
    assert_eq!(pods[0].description.as_deref(), Some("default (Running)"));

    let namespaces = parse_kubectl_json_list(
        r#"{"items":[{"metadata":{"name":"default"},"status":{"phase":"Active"}},{"metadata":{"name":"kube-system"},"status":{"phase":"Active"}}]}"#,
        K8sKind::Namespaces,
    );
    assert_eq!(texts(&namespaces), vec!["default", "kube-system"]);
    assert_eq!(namespaces[0].description.as_deref(), Some("Active"));

    let nodes = parse_kubectl_json_list(
        r#"{"items":[{"metadata":{"name":"kind-control-plane"}},{"metadata":{"name":"kind-worker"}}]}"#,
        K8sKind::Nodes,
    );
    assert_eq!(texts(&nodes), vec!["kind-control-plane", "kind-worker"]);

    let services = parse_kubectl_json_list(
        r#"{"items":[{"metadata":{"name":"kubernetes","namespace":"default"},"spec":{"type":"ClusterIP","clusterIP":"10.96.0.1"}}]}"#,
        K8sKind::Services,
    );
    assert_eq!(texts(&services), vec!["kubernetes"]);
    assert_eq!(
        services[0].description.as_deref(),
        Some("default ClusterIP 10.96.0.1")
    );

    for suggestion in pods
        .iter()
        .chain(namespaces.iter())
        .chain(nodes.iter())
        .chain(services.iter())
    {
        assert_eq!(suggestion.kind, SuggestionKind::ProviderValue);
        assert_eq!(suggestion.source, SuggestionSource::Provider);
    }
}

#[test]
fn parses_recorded_kubectl_line_outputs() {
    let resources =
        parse_kubectl_lines("pods\nservices\napps/v1/deployments\n", K8sKind::Resources);
    assert_eq!(
        texts(&resources),
        vec!["pods", "services", "apps/v1/deployments"]
    );

    let legacy_resources = parse_kubectl_lines(
        "NAME SHORTNAMES APIVERSION NAMESPACED KIND\npods po v1 true Pod\nservices svc v1 true Service\n",
        K8sKind::Resources,
    );
    assert_eq!(texts(&legacy_resources), vec!["pods", "services"]);

    let contexts = parse_kubectl_lines("kind-dev\nprod-admin\n", K8sKind::Contexts);
    assert_eq!(texts(&contexts), vec!["kind-dev", "prod-admin"]);
}

#[test]
fn kubectl_empty_output_yields_no_suggestions() {
    for kind in [
        K8sKind::Resources,
        K8sKind::Contexts,
        K8sKind::Pods,
        K8sKind::Namespaces,
        K8sKind::Nodes,
        K8sKind::Services,
    ] {
        assert!(parse_kubectl_lines("", kind).is_empty());
        assert!(parse_kubectl_json_list("", kind).is_empty());
    }
}

#[test]
fn kubectl_malformed_json_yields_no_suggestions() {
    assert!(parse_kubectl_json_list("not json", K8sKind::Pods).is_empty());
    assert!(parse_kubectl_json_list("{\"items\":[", K8sKind::Services).is_empty());
}

#[tokio::test]
async fn kubectl_missing_binary_falls_back_to_empty() {
    let tmp = tempfile::TempDir::new().unwrap();
    let ctx = ctx(tmp.path());

    for result in [
        K8sResources
            .generate_with_binary(&ctx, "/nonexistent/kubectl-for-provider-test")
            .await,
        K8sPods
            .generate_with_binary(&ctx, "/nonexistent/kubectl-for-provider-test")
            .await,
        K8sNamespaces
            .generate_with_binary(&ctx, "/nonexistent/kubectl-for-provider-test")
            .await,
        K8sContexts
            .generate_with_binary(&ctx, "/nonexistent/kubectl-for-provider-test")
            .await,
        K8sNodes
            .generate_with_binary(&ctx, "/nonexistent/kubectl-for-provider-test")
            .await,
        K8sServices
            .generate_with_binary(&ctx, "/nonexistent/kubectl-for-provider-test")
            .await,
    ] {
        assert!(matches!(result, Ok(ref suggestions) if suggestions.is_empty()));
    }
}

#[tokio::test]
async fn kubectl_spawn_honors_kubeconfig_from_provider_ctx() {
    let tmp = tempfile::TempDir::new().unwrap();
    let expected_kubeconfig = tmp.path().join("config");
    let fake_kubectl = tmp.path().join("kubectl");
    let script = format!(
        "#!/bin/sh\nif [ \"$KUBECONFIG\" != \"{}\" ]; then exit 42; fi\nprintf '%s\\n' '{{\"items\":[{{\"metadata\":{{\"name\":\"ctx-pod\",\"namespace\":\"default\"}},\"status\":{{\"phase\":\"Running\"}}}}]}}'\n",
        expected_kubeconfig.display()
    );
    std::fs::write(&fake_kubectl, script).unwrap();

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&fake_kubectl).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&fake_kubectl, perms).unwrap();
    }

    let ctx = ctx_with_kubeconfig(tmp.path(), expected_kubeconfig);
    let suggestions = K8sPods
        .generate_with_binary(&ctx, fake_kubectl.to_str().unwrap())
        .await
        .unwrap();

    assert_eq!(texts(&suggestions), vec!["ctx-pod"]);
}

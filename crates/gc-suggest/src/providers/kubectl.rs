// kubectl native providers.
//
// Cluster-object providers use `kubectl get <resource> -o json` and
// project `metadata.name`. Resource and context providers use the
// stable one-name-per-line outputs. All subprocess paths degrade to an
// empty suggestion list so a missing cluster or missing binary never
// stalls completion.

use std::path::Path;
use std::time::Duration;

use anyhow::Result;
use serde::Deserialize;

use super::util::{parse_json_root, spawn_with_timeout};
use super::version_probe;
use super::{Provider, ProviderCtx};
use crate::types::{Suggestion, SuggestionKind, SuggestionSource};

const KUBECTL_TIMEOUT_MS: u64 = 2_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum K8sKind {
    Resources,
    Pods,
    Namespaces,
    Contexts,
    Nodes,
    Services,
}

impl K8sKind {
    fn args(self) -> &'static [&'static str] {
        match self {
            Self::Resources => &["api-resources", "-o", "name"],
            Self::Pods => &["get", "pods", "-o", "json"],
            Self::Namespaces => &["get", "namespaces", "-o", "json"],
            Self::Contexts => &["config", "get-contexts", "-o", "name"],
            Self::Nodes => &["get", "nodes", "-o", "json"],
            Self::Services => &["get", "services", "-o", "json"],
        }
    }

    fn legacy_args(self) -> Option<&'static [&'static str]> {
        match self {
            Self::Resources => Some(&["api-resources"]),
            _ => None,
        }
    }

    fn is_line_output(self) -> bool {
        matches!(self, Self::Resources | Self::Contexts)
    }
}

#[derive(Debug, Deserialize)]
struct KubernetesList {
    #[serde(default)]
    items: Vec<KubernetesItem>,
}

#[derive(Debug, Deserialize)]
struct KubernetesItem {
    #[serde(default)]
    metadata: Option<KubernetesMetadata>,
    #[serde(default)]
    status: Option<KubernetesStatus>,
    #[serde(default)]
    spec: Option<KubernetesSpec>,
}

#[derive(Debug, Deserialize)]
struct KubernetesMetadata {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    namespace: Option<String>,
}

#[derive(Debug, Deserialize)]
struct KubernetesStatus {
    #[serde(default)]
    phase: Option<String>,
}

#[derive(Debug, Deserialize)]
struct KubernetesSpec {
    #[serde(default, rename = "type")]
    service_type: Option<String>,
    #[serde(default, rename = "clusterIP")]
    cluster_ip: Option<String>,
}

pub(crate) fn parse_kubectl_lines(stdout: &str, kind: K8sKind) -> Vec<Suggestion> {
    if !kind.is_line_output() {
        return Vec::new();
    }

    stdout
        .lines()
        .filter_map(|line| line_to_suggestion(line, kind))
        .collect()
}

fn line_to_suggestion(line: &str, kind: K8sKind) -> Option<Suggestion> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }

    let text = match kind {
        K8sKind::Resources => {
            let first = trimmed.split_whitespace().next()?;
            if first.eq_ignore_ascii_case("NAME") {
                return None;
            }
            first
        }
        K8sKind::Contexts => trimmed,
        _ => return None,
    };

    Some(provider_suggestion(
        text.to_string(),
        Some(match kind {
            K8sKind::Resources => "kubernetes resource".to_string(),
            K8sKind::Contexts => "kubernetes context".to_string(),
            _ => return None,
        }),
    ))
}

pub(crate) fn parse_kubectl_json_list(stdout: &str, kind: K8sKind) -> Vec<Suggestion> {
    if kind.is_line_output() {
        return Vec::new();
    }

    let Ok(list) = parse_json_root::<KubernetesList>(stdout) else {
        return Vec::new();
    };

    list.items
        .into_iter()
        .filter_map(|item| item_to_suggestion(item, kind))
        .collect()
}

fn item_to_suggestion(item: KubernetesItem, kind: K8sKind) -> Option<Suggestion> {
    let metadata = item.metadata?;
    let name = clean(metadata.name.as_deref())?.to_string();
    let description = match kind {
        K8sKind::Pods => pod_description(metadata.namespace.as_deref(), item.status.as_ref()),
        K8sKind::Namespaces => item
            .status
            .as_ref()
            .and_then(|status| clean(status.phase.as_deref()))
            .map(str::to_string),
        K8sKind::Nodes => None,
        K8sKind::Services => service_description(metadata.namespace.as_deref(), item.spec.as_ref()),
        K8sKind::Resources | K8sKind::Contexts => None,
    };

    Some(provider_suggestion(name, description))
}

fn pod_description(namespace: Option<&str>, status: Option<&KubernetesStatus>) -> Option<String> {
    let namespace = clean(namespace);
    let phase = status.and_then(|status| clean(status.phase.as_deref()));
    match (namespace, phase) {
        (Some(namespace), Some(phase)) => Some(format!("{namespace} ({phase})")),
        (Some(namespace), None) => Some(namespace.to_string()),
        (None, Some(phase)) => Some(phase.to_string()),
        (None, None) => None,
    }
}

fn service_description(namespace: Option<&str>, spec: Option<&KubernetesSpec>) -> Option<String> {
    let spec = spec?;
    join_description([
        clean(namespace),
        clean(spec.service_type.as_deref()),
        clean(spec.cluster_ip.as_deref()),
    ])
}

fn provider_suggestion(text: String, description: Option<String>) -> Suggestion {
    Suggestion {
        text,
        description,
        kind: SuggestionKind::ProviderValue,
        source: SuggestionSource::Provider,
        ..Default::default()
    }
}

fn join_description<'a, I>(parts: I) -> Option<String>
where
    I: IntoIterator<Item = Option<&'a str>>,
{
    let parts: Vec<&str> = parts.into_iter().filter_map(clean).collect();
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" "))
    }
}

fn clean(value: Option<&str>) -> Option<&str> {
    let value = value?.trim();
    if value.is_empty() || value == "<none>" {
        None
    } else {
        Some(value)
    }
}

async fn run_kubectl_with_args(
    cwd: &Path,
    env: &std::collections::HashMap<String, String>,
    binary: &str,
    args: &'static [&'static str],
) -> Option<String> {
    let pass_env = env.get("KUBECONFIG").map(|kubeconfig| {
        std::collections::HashMap::from([("KUBECONFIG".to_string(), kubeconfig.clone())])
    });

    match spawn_with_timeout(
        cwd,
        binary,
        args.iter().copied(),
        pass_env.as_ref(),
        Duration::from_millis(KUBECTL_TIMEOUT_MS),
    )
    .await
    {
        Ok(stdout) => Some(stdout),
        Err(error) => {
            tracing::warn!(binary, error = %error, "kubectl provider command failed");
            None
        }
    }
}

async fn generate_kubectl_with_binary(
    ctx: &ProviderCtx,
    binary: &str,
    kind: K8sKind,
) -> Result<Vec<Suggestion>> {
    let use_legacy_resources = if kind == K8sKind::Resources {
        matches!(
            version_probe::probe_version_with_args(
                binary,
                ["version", "--client", "--short"],
                "1.11",
            )
            .await,
            Some(false)
        )
    } else {
        false
    };
    let primary_args = if use_legacy_resources {
        kind.legacy_args().unwrap_or(kind.args())
    } else {
        kind.args()
    };

    let stdout = match run_kubectl_with_args(&ctx.cwd, ctx.env.as_ref(), binary, primary_args).await
    {
        Some(stdout) => stdout,
        None if !use_legacy_resources => match kind.legacy_args() {
            Some(args) => {
                match run_kubectl_with_args(&ctx.cwd, ctx.env.as_ref(), binary, args).await {
                    Some(stdout) => stdout,
                    None => return Ok(Vec::new()),
                }
            }
            None => return Ok(Vec::new()),
        },
        None => return Ok(Vec::new()),
    };

    if kind.is_line_output() {
        Ok(parse_kubectl_lines(&stdout, kind))
    } else {
        Ok(parse_kubectl_json_list(&stdout, kind))
    }
}

macro_rules! kubectl_provider {
    ($provider:ident, $name:literal, $kind:expr) => {
        pub struct $provider;

        impl Provider for $provider {
            fn name(&self) -> &'static str {
                $name
            }

            async fn generate(&self, ctx: &ProviderCtx) -> Result<Vec<Suggestion>> {
                self.generate_with_binary(ctx, "kubectl").await
            }
        }

        impl $provider {
            pub(crate) async fn generate_with_binary(
                &self,
                ctx: &ProviderCtx,
                binary: &str,
            ) -> Result<Vec<Suggestion>> {
                generate_kubectl_with_binary(ctx, binary, $kind).await
            }
        }
    };
}

kubectl_provider!(K8sResources, "k8s_resources", K8sKind::Resources);
kubectl_provider!(K8sPods, "k8s_pods", K8sKind::Pods);
kubectl_provider!(K8sNamespaces, "k8s_namespaces", K8sKind::Namespaces);
kubectl_provider!(K8sContexts, "k8s_contexts", K8sKind::Contexts);
kubectl_provider!(K8sNodes, "k8s_nodes", K8sKind::Nodes);
kubectl_provider!(K8sServices, "k8s_services", K8sKind::Services);

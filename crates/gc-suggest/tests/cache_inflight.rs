use std::collections::HashSet;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use std::time::Duration;

use gc_suggest::cache::{CacheKey, GeneratorCache};
use gc_suggest::{Suggestion, SuggestionSource};

fn aws_key() -> CacheKey {
    CacheKey::AwsSdk {
        service: "iam".into(),
        operation: "ListRoles".into(),
        params_hash: 42,
        profile: Some("prod".into()),
        region: Some("us-east-1".into()),
    }
}

fn suggestions(label: &str) -> Vec<Suggestion> {
    vec![Suggestion {
        text: label.into(),
        source: SuggestionSource::Provider,
        ..Default::default()
    }]
}

fn suggestion_texts(suggestions: &[Suggestion]) -> Vec<&str> {
    suggestions.iter().map(|s| s.text.as_str()).collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_misses_for_same_key_issue_one_backend_call() {
    let cache = Arc::new(GeneratorCache::new());
    let calls = Arc::new(AtomicUsize::new(0));
    let key = aws_key();

    let mut handles = Vec::new();
    for _ in 0..100 {
        let cache = Arc::clone(&cache);
        let calls = Arc::clone(&calls);
        let key = key.clone();
        handles.push(tokio::spawn(async move {
            cache
                .get_or_try_insert_with(key, Duration::from_secs(60), || async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(25)).await;
                    Ok::<_, ()>(suggestions("role-a"))
                })
                .await
                .unwrap()
        }));
    }

    for handle in handles {
        assert_eq!(suggestion_texts(&handle.await.unwrap()), vec!["role-a"]);
    }
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn expired_entry_concurrent_callers_issue_one_fresh_backend_call() {
    let cache = Arc::new(GeneratorCache::new());
    let key = aws_key();
    cache.insert(key.clone(), suggestions("stale"), Duration::from_millis(1));
    tokio::time::sleep(Duration::from_millis(5)).await;

    let calls = Arc::new(AtomicUsize::new(0));
    let mut handles = Vec::new();
    for _ in 0..2 {
        let cache = Arc::clone(&cache);
        let calls = Arc::clone(&calls);
        let key = key.clone();
        handles.push(tokio::spawn(async move {
            cache
                .get_or_try_insert_with(key, Duration::from_secs(60), || async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(25)).await;
                    Ok::<_, ()>(suggestions("fresh"))
                })
                .await
                .unwrap()
        }));
    }

    for handle in handles {
        assert_eq!(suggestion_texts(&handle.await.unwrap()), vec!["fresh"]);
    }
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn aws_sdk_cache_key_differs_by_service_operation_params_profile_and_region() {
    let base = aws_key();
    let variants = [
        CacheKey::AwsSdk {
            service: "s3".into(),
            operation: "ListRoles".into(),
            params_hash: 42,
            profile: Some("prod".into()),
            region: Some("us-east-1".into()),
        },
        CacheKey::AwsSdk {
            service: "iam".into(),
            operation: "ListUsers".into(),
            params_hash: 42,
            profile: Some("prod".into()),
            region: Some("us-east-1".into()),
        },
        CacheKey::AwsSdk {
            service: "iam".into(),
            operation: "ListRoles".into(),
            params_hash: 43,
            profile: Some("prod".into()),
            region: Some("us-east-1".into()),
        },
        CacheKey::AwsSdk {
            service: "iam".into(),
            operation: "ListRoles".into(),
            params_hash: 42,
            profile: Some("dev".into()),
            region: Some("us-east-1".into()),
        },
        CacheKey::AwsSdk {
            service: "iam".into(),
            operation: "ListRoles".into(),
            params_hash: 42,
            profile: Some("prod".into()),
            region: Some("eu-west-1".into()),
        },
    ];

    let mut keys = HashSet::from([base.clone()]);
    for variant in variants {
        assert_ne!(base, variant);
        assert!(keys.insert(variant));
    }
}

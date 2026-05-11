use std::path::Path;

use serde_json::Value;

fn spec_value(name: &str) -> Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../specs")
        .join(format!("{name}.json"));
    let raw = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!("failed to read {}: {e}", path.display());
    });
    serde_json::from_str(&raw).unwrap_or_else(|e| {
        panic!("failed to parse {}: {e}", path.display());
    })
}

fn collect_static_extracted<'a>(value: &'a Value, out: &mut Vec<&'a Value>) {
    match value {
        Value::Object(map) => {
            if map
                .get("_static_extracted_subprocess")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                out.push(value);
            }
            for child in map.values() {
                collect_static_extracted(child, out);
            }
        }
        Value::Array(items) => {
            for child in items {
                collect_static_extracted(child, out);
            }
        }
        _ => {}
    }
}

#[test]
fn apt_static_extract_generators_are_native_script_transforms() {
    let apt = spec_value("apt");
    let mut generators = Vec::new();
    collect_static_extracted(&apt, &mut generators);

    assert_eq!(
        generators.len(),
        7,
        "ux-11 should mark the statically extracted apt subprocess generators"
    );

    for gen in generators {
        assert_eq!(
            gen.get("script").and_then(Value::as_array).map(Vec::len),
            Some(2),
            "static extract must retain a literal script argv: {gen:?}"
        );
        assert!(
            gen.get("transforms")
                .and_then(Value::as_array)
                .is_some_and(|items| !items.is_empty()),
            "static extract must have transforms: {gen:?}"
        );
        assert_eq!(gen.get("requires_js"), None);
        assert_eq!(gen.get("js_runtime"), None);
    }
}

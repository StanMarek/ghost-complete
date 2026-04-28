use gc_buffer::{CommandContext, QuoteState};
use gc_suggest::specs::{parse_spec_checked_and_sanitized, resolve_spec, validate_spec_generators};
use proptest::prelude::*;

fn arb_name_value() -> impl Strategy<Value = serde_json::Value> {
    prop_oneof![
        // Single string
        "[a-zA-Z][a-zA-Z0-9]{0,8}".prop_map(serde_json::Value::String),
        // Array of 0..3 names
        prop::collection::vec("[a-zA-Z][a-zA-Z0-9]{0,8}", 0..=3).prop_map(|names| {
            serde_json::Value::Array(names.into_iter().map(serde_json::Value::String).collect())
        }),
    ]
}

fn arb_object_entry() -> impl Strategy<Value = serde_json::Value> {
    (
        arb_name_value(),
        prop::option::of("[a-zA-Z ]{0,30}"),
        prop::option::of(prop_oneof![
            Just("subcommand"),
            Just("option"),
            Just("file"),
            Just("folder"),
            Just("arg"),
            Just("made_up"),
        ]),
        prop::option::of(0i32..=120),
        any::<bool>(),
    )
        .prop_map(|(name, desc, kind, prio, hidden)| {
            let mut m = serde_json::Map::new();
            m.insert("name".to_string(), name);
            if let Some(d) = desc {
                m.insert("description".to_string(), serde_json::Value::String(d));
            }
            if let Some(k) = kind {
                m.insert("type".to_string(), serde_json::Value::String(k.to_string()));
            }
            if let Some(p) = prio {
                m.insert(
                    "priority".to_string(),
                    serde_json::Value::Number(serde_json::Number::from(p)),
                );
            }
            m.insert("hidden".to_string(), serde_json::Value::Bool(hidden));
            serde_json::Value::Object(m)
        })
}

fn arb_dirty_plain() -> impl Strategy<Value = serde_json::Value> {
    prop_oneof![
        Just(serde_json::Value::String("\u{001b}[31mred".to_string())),
        Just(serde_json::Value::String("tab\there".to_string())),
        // whitespace-only → empty-name pruning
        Just(serde_json::Value::String("   ".to_string())),
        Just(serde_json::Value::String("\u{0000}null".to_string())),
    ]
}

fn arb_entry() -> impl Strategy<Value = serde_json::Value> {
    prop_oneof![
        "[a-zA-Z][a-zA-Z0-9]{0,8}".prop_map(serde_json::Value::String),
        arb_object_entry(),
        arb_dirty_plain(),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 256, ..Default::default() })]

    #[test]
    fn static_suggestion_entries_never_panic_or_emit_empty_text(
        entries in prop::collection::vec(arb_entry(), 0..=8)
    ) {
        let entries_json = serde_json::Value::Array(entries);
        let spec_json = serde_json::json!({
            "name": "fakecmd",
            "args": [{
                "name": "x",
                "suggestions": entries_json,
            }],
        })
        .to_string();

        let mut spec = match parse_spec_checked_and_sanitized(&spec_json) {
            Ok(s) => s,
            // Some property-generated input may legitimately fail to parse
            Err(_) => return Ok(()),
        };
        let _warnings = validate_spec_generators(&mut spec);

        let ctx = CommandContext {
            command: Some("fakecmd".into()),
            args: vec![],
            current_word: String::new(),
            word_index: 1,
            is_flag: false,
            is_long_flag: false,
            preceding_flag: None,
            in_pipe: false,
            in_redirect: false,
            quote_state: QuoteState::None,
            is_first_segment: true,
        };
        let res = resolve_spec(&spec, &ctx);
        for s in &res.static_suggestions {
            prop_assert!(!s.text.is_empty(), "static suggestion has empty text: {:?}", s);
        }
    }
}

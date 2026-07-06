//! Property-based tests for core type serialization round-trips.
//!
//! Feature: rust-agent-framework, Property 1: Core type serialization round-trip
//! Validates: Requirements 2.6, 3.9
//!
//! Feature: rust-agent-framework, Property 2: RunState serialization round-trip
//! Validates: Requirements 7.6
//!
//! Feature: rust-agent-framework, Property 3: RunState deserialization robustness
//! Validates: Requirements 7.5

use proptest::prelude::*;
use serde_json;

use agent_core::{
    CompactionState, ContentBlock, Message, PendingApproval, RunError, RunState, StreamChunk,
    StopReason, ToolUseBlock, Usage, SCHEMA_VERSION,
};

// --- Arbitrary strategy implementations ---

/// Strategy for generating arbitrary JSON values (limited depth to avoid blowup).
fn arb_json_value() -> impl Strategy<Value = serde_json::Value> {
    let leaf = prop_oneof![
        Just(serde_json::Value::Null),
        any::<bool>().prop_map(serde_json::Value::Bool),
        // Use finite f64 values only to avoid NaN/Infinity which don't round-trip in JSON
        any::<i64>().prop_map(|n| serde_json::Value::Number(serde_json::Number::from(n))),
        "[a-zA-Z0-9 _-]{0,50}".prop_map(|s| serde_json::Value::String(s)),
    ];

    leaf.prop_recursive(
        3,  // depth
        64, // max nodes
        10, // items per collection
        |inner| {
            prop_oneof![
                prop::collection::vec(inner.clone(), 0..5)
                    .prop_map(serde_json::Value::Array),
                prop::collection::hash_map("[a-zA-Z_][a-zA-Z0-9_]{0,10}", inner, 0..5)
                    .prop_map(|m| serde_json::Value::Object(
                        m.into_iter().collect()
                    )),
            ]
        },
    )
}

/// Strategy for generating arbitrary Usage values.
fn arb_usage() -> impl Strategy<Value = Usage> {
    (any::<u64>(), any::<u64>(), proptest::option::of(any::<u64>())).prop_map(
        |(input_tokens, output_tokens, cache_read_tokens)| Usage {
            input_tokens,
            output_tokens,
            cache_read_tokens,
        },
    )
}

/// Strategy for generating arbitrary ToolUseBlock values.
fn arb_tool_use_block() -> impl Strategy<Value = ToolUseBlock> {
    (
        "[a-zA-Z0-9_-]{1,20}",  // id
        "[a-zA-Z_][a-zA-Z0-9_]{0,20}",  // name
        arb_json_value(),  // input
    )
        .prop_map(|(id, name, input)| ToolUseBlock { id, name, input })
}

/// Strategy for generating arbitrary ContentBlock values.
fn arb_content_block() -> impl Strategy<Value = ContentBlock> {
    prop_oneof![
        // Text variant
        "[^\x00]{0,100}".prop_map(|text| ContentBlock::Text { text }),
        // Image variant
        (
            "(image/png|image/jpeg|image/gif|image/webp)",
            "[a-zA-Z0-9+/=]{0,50}",
            "(base64|url)",
        )
            .prop_map(|(media_type, data, source_type)| ContentBlock::Image {
                media_type,
                data,
                source_type,
            }),
        // ToolUse variant
        arb_tool_use_block().prop_map(|block| ContentBlock::ToolUse { block }),
    ]
}

/// Strategy for generating arbitrary Message values.
fn arb_message() -> impl Strategy<Value = Message> {
    prop_oneof![
        // System variant
        "[^\x00]{0,200}".prop_map(|content| Message::System { content }),
        // User variant
        prop::collection::vec(arb_content_block(), 1..5)
            .prop_map(|content| Message::User { content }),
        // Assistant variant
        (
            prop::collection::vec(arb_content_block(), 1..5),
            proptest::option::of(arb_usage()),
        )
            .prop_map(|(content, usage)| Message::Assistant { content, usage }),
        // ToolResult variant
        (
            "[a-zA-Z0-9_-]{1,20}",
            "[^\x00]{0,100}",
            any::<bool>(),
        )
            .prop_map(|(tool_use_id, content, is_error)| Message::ToolResult {
                tool_use_id,
                content,
                is_error,
            }),
    ]
}

/// Strategy for generating arbitrary StopReason values.
fn arb_stop_reason() -> impl Strategy<Value = StopReason> {
    prop_oneof![
        Just(StopReason::EndTurn),
        Just(StopReason::ToolUse),
        Just(StopReason::MaxTokens),
        Just(StopReason::StopSequence),
        Just(StopReason::ContentFilter),
    ]
}

/// Strategy for generating arbitrary StreamChunk values.
fn arb_stream_chunk() -> impl Strategy<Value = StreamChunk> {
    prop_oneof![
        // TextDelta
        "[^\x00]{0,100}".prop_map(|text| StreamChunk::TextDelta { text }),
        // ThinkingDelta
        "[^\x00]{0,100}".prop_map(|text| StreamChunk::ThinkingDelta { text }),
        // ToolUseStart
        ("[a-zA-Z0-9_-]{1,20}", "[a-zA-Z_][a-zA-Z0-9_]{0,20}")
            .prop_map(|(id, name)| StreamChunk::ToolUseStart { id, name }),
        // ToolUseInputDelta
        ("[a-zA-Z0-9_-]{1,20}", "[^\x00]{0,50}")
            .prop_map(|(id, delta)| StreamChunk::ToolUseInputDelta { id, delta }),
        // ToolUseEnd
        ("[a-zA-Z0-9_-]{1,20}", arb_json_value())
            .prop_map(|(id, input)| StreamChunk::ToolUseEnd { id, input }),
        // MessageStop
        (arb_stop_reason(), arb_usage())
            .prop_map(|(stop_reason, usage)| StreamChunk::MessageStop { stop_reason, usage }),
    ]
}

/// Strategy for generating arbitrary PendingApproval values.
fn arb_pending_approval() -> impl Strategy<Value = PendingApproval> {
    (
        "[a-zA-Z_][a-zA-Z0-9_]{0,20}", // tool_name
        arb_json_value(),               // tool_input
        "[a-zA-Z0-9_-]{1,20}",          // request_id
    )
        .prop_map(|(tool_name, tool_input, request_id)| PendingApproval {
            tool_name,
            tool_input,
            request_id,
        })
}

/// Strategy for generating arbitrary CompactionState values.
fn arb_compaction_state() -> impl Strategy<Value = CompactionState> {
    (
        any::<u32>(),                       // total_compactions
        0..1000usize,                       // messages_removed
        proptest::option::of(any::<u32>()), // last_compaction_turn
        0..10u32,                           // consecutive_failures
        any::<bool>(),                      // circuit_broken
        proptest::option::of(0..500000usize), // last_token_count
    )
        .prop_map(
            |(total_compactions, messages_removed, last_compaction_turn, consecutive_failures, circuit_broken, last_token_count)| CompactionState {
                total_compactions,
                messages_removed,
                last_compaction_turn,
                consecutive_failures,
                circuit_broken,
                last_token_count,
            },
        )
}

/// Strategy for generating arbitrary RunState values.
///
/// Note: schema_version is always set to SCHEMA_VERSION since deserialize()
/// validates the version and rejects unrecognized versions.
/// total_cost_usd uses finite f64 values to ensure JSON round-trip correctness.
fn arb_run_state() -> impl Strategy<Value = RunState> {
    (
        "[a-zA-Z0-9_-]{1,30}",                          // run_id
        proptest::option::of("[a-zA-Z0-9_-]{1,30}"),     // session_id
        prop::collection::vec(arb_message(), 0..5),      // messages
        any::<u32>(),                                    // current_turn
        proptest::option::of(1..100u32),                 // max_turns
        // Generate f64 as integer cents divided by 100 to ensure JSON round-trip
        // (avoids floating-point precision issues with arbitrary f64 in JSON)
        (-100_000i64..100_000i64).prop_map(|cents| cents as f64 / 100.0), // total_cost_usd
        arb_usage(),                                     // total_usage
        prop::collection::vec(arb_pending_approval(), 0..3), // pending_approvals
        arb_compaction_state(),                          // compaction_state
        "[a-zA-Z0-9_-]{0,30}",                          // trace_id
    )
        .prop_map(
            |(
                run_id,
                session_id,
                messages,
                current_turn,
                max_turns,
                total_cost_usd,
                total_usage,
                pending_approvals,
                compaction_state,
                trace_id,
            )| {
                RunState {
                    run_id,
                    session_id,
                    messages,
                    current_turn,
                    max_turns,
                    total_cost_usd,
                    total_usage,
                    pending_approvals,
                    compaction_state,
                    trace_id,
                    schema_version: SCHEMA_VERSION.to_string(),
                }
            },
        )
}

// --- Property tests ---

proptest! {
    /// **Validates: Requirements 2.6**
    ///
    /// For any valid Message value, serializing to JSON then deserializing
    /// from JSON produces a value equal to the original.
    #[test]
    fn message_serialization_roundtrip(msg in arb_message()) {
        let json = serde_json::to_string(&msg)
            .expect("Message serialization should not fail");
        let deserialized: Message = serde_json::from_str(&json)
            .expect("Message deserialization should not fail");
        prop_assert_eq!(&msg, &deserialized);
    }

    /// **Validates: Requirements 3.9**
    ///
    /// For any valid StreamChunk value, serializing to JSON then deserializing
    /// from JSON produces a value equal to the original.
    #[test]
    fn stream_chunk_serialization_roundtrip(chunk in arb_stream_chunk()) {
        let json = serde_json::to_string(&chunk)
            .expect("StreamChunk serialization should not fail");
        let deserialized: StreamChunk = serde_json::from_str(&json)
            .expect("StreamChunk deserialization should not fail");
        prop_assert_eq!(&chunk, &deserialized);
    }

    /// **Validates: Requirements 2.6**
    ///
    /// For any valid ContentBlock value, serializing to JSON then deserializing
    /// from JSON produces a value equal to the original.
    #[test]
    fn content_block_serialization_roundtrip(block in arb_content_block()) {
        let json = serde_json::to_string(&block)
            .expect("ContentBlock serialization should not fail");
        let deserialized: ContentBlock = serde_json::from_str(&json)
            .expect("ContentBlock deserialization should not fail");
        prop_assert_eq!(&block, &deserialized);
    }

    /// **Validates: Requirements 2.6**
    ///
    /// For any valid Usage value, serializing to JSON then deserializing
    /// from JSON produces a value equal to the original.
    #[test]
    fn usage_serialization_roundtrip(usage in arb_usage()) {
        let json = serde_json::to_string(&usage)
            .expect("Usage serialization should not fail");
        let deserialized: Usage = serde_json::from_str(&json)
            .expect("Usage deserialization should not fail");
        prop_assert_eq!(&usage, &deserialized);
    }

    /// **Validates: Requirements 2.6**
    ///
    /// For any valid ToolUseBlock value, serializing to JSON then deserializing
    /// from JSON produces a value equal to the original.
    #[test]
    fn tool_use_block_serialization_roundtrip(block in arb_tool_use_block()) {
        let json = serde_json::to_string(&block)
            .expect("ToolUseBlock serialization should not fail");
        let deserialized: ToolUseBlock = serde_json::from_str(&json)
            .expect("ToolUseBlock deserialization should not fail");
        prop_assert_eq!(&block, &deserialized);
    }

    /// **Validates: Requirements 7.6**
    ///
    /// For any valid RunState instance, calling serialize() followed by
    /// deserialize() on the resulting bytes produces a RunState that is
    /// equal to the original via the derived PartialEq implementation.
    #[test]
    fn run_state_serialization_roundtrip(state in arb_run_state()) {
        let bytes = state.serialize()
            .expect("RunState serialization should not fail");
        let restored = RunState::deserialize(&bytes)
            .expect("RunState deserialization should not fail");
        prop_assert_eq!(&state, &restored);
    }
}


// --- Property 3: RunState deserialization robustness ---

proptest! {
    /// **Validates: Requirements 7.5**
    ///
    /// Feature: rust-agent-framework, Property 3: RunState deserialization robustness
    ///
    /// For any arbitrary byte slice (including random, malformed, or zero-length bytes),
    /// calling RunState::deserialize() shall return a Result::Err without panicking.
    #[test]
    fn run_state_deserialize_arbitrary_bytes_never_panics(bytes in prop::collection::vec(any::<u8>(), 0..1024)) {
        // The key property: deserialize never panics regardless of input.
        // It should always return Err for random bytes (the probability of random bytes
        // being valid RunState JSON with correct schema version is effectively zero).
        let result = RunState::deserialize(&bytes);
        // Result must be Err (random bytes won't be valid RunState JSON)
        prop_assert!(result.is_err());
        // Verify it's a Serialization error variant
        match result.unwrap_err() {
            RunError::Serialization(_) => {} // expected
            other => prop_assert!(false, "Expected Serialization error, got: {:?}", other),
        }
    }

    /// **Validates: Requirements 7.5**
    ///
    /// For any valid JSON value that does NOT conform to the RunState schema,
    /// RunState::deserialize() shall return Err without panicking.
    #[test]
    fn run_state_deserialize_wrong_schema_json_returns_err(json_val in arb_json_value()) {
        let bytes = serde_json::to_vec(&json_val).unwrap();
        let result = RunState::deserialize(&bytes);
        // Arbitrary JSON values will not have the correct RunState fields,
        // so this should always return Err.
        prop_assert!(result.is_err());
        match result.unwrap_err() {
            RunError::Serialization(_) => {} // expected
            other => prop_assert!(false, "Expected Serialization error, got: {:?}", other),
        }
    }

    /// **Validates: Requirements 7.5**
    ///
    /// For any RunState serialized with a mutated (non-current) schema_version,
    /// RunState::deserialize() shall return Err indicating version mismatch.
    #[test]
    fn run_state_deserialize_wrong_schema_version_returns_err(
        version in "[0-9]{1,3}\\.[0-9]{1,3}\\.[0-9]{1,3}"
    ) {
        // Skip if the generated version happens to match the current one
        prop_assume!(version != SCHEMA_VERSION);

        let mut state = RunState::new("test-run".into(), None, None);
        state.schema_version = version;
        let bytes = serde_json::to_vec(&state).unwrap();

        let result = RunState::deserialize(&bytes);
        prop_assert!(result.is_err());
        match result.unwrap_err() {
            RunError::Serialization(msg) => {
                prop_assert!(msg.contains("unrecognized schema version"));
            }
            other => prop_assert!(false, "Expected Serialization error, got: {:?}", other),
        }
    }
}

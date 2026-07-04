//! Permission engine with a 4-layer evaluation pipeline.
//!
//! The `PermissionEngine` evaluates whether a tool call is permitted before
//! execution, using a layered approach that short-circuits at the first
//! definitive decision.
//!
//! ## Layer Order
//!
//! 1. **Mode check** — If `Bypass`, allow all; if `DenyAll`, deny all.
//! 2. **Static rules** — If tool name matches `static_deny`, deny; if matches `static_allow`, allow.
//! 3. **Session rules** — If tool name has been granted session-level allow via `grant_session_allow`, allow.
//! 4. **Tool approval requirement** — Evaluate the tool's `ApprovalRequirement` (Never → Allow, Always → NeedsApproval, Conditional → NeedsApproval with context).

use std::sync::Arc;

use tokio::sync::RwLock;

use crate::pattern::ToolPattern;
use crate::settings::MergedPolicy;
use crate::tool::ApprovalRequirement;

/// The decision returned by the permission engine after evaluating all layers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PermissionDecision {
    /// The tool call is permitted.
    Allow {
        /// Optional reason explaining why the call was allowed.
        reason: Option<String>,
    },
    /// The tool call is denied.
    Deny {
        /// A human-readable message describing why the call was denied.
        message: String,
        /// The specific reason/category for denial.
        reason: String,
    },
    /// The tool call requires interactive user approval before proceeding.
    NeedsApproval {
        /// Description of what approval is needed for.
        description: String,
        /// Unique identifier for this approval request.
        call_id: String,
        /// Additional context about the tool call.
        context: String,
    },
}

/// The operating mode of the permission engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionMode {
    /// All tool calls are allowed without further evaluation.
    Bypass,
    /// Normal evaluation through the full pipeline.
    Normal,
    /// All tool calls are denied without further evaluation.
    DenyAll,
}

/// A 4-layer permission evaluation engine.
///
/// Evaluates tool call permissions by checking layers in order and
/// short-circuiting at the first definitive decision.
///
/// # Example
///
/// ```
/// use agent_core::permission::{PermissionEngine, PermissionMode, PermissionDecision};
/// use agent_core::tool::ApprovalRequirement;
///
/// let mut engine = PermissionEngine::new(PermissionMode::Normal)
///     .with_static_allow(vec!["read_file".to_string()])
///     .with_static_deny(vec!["dangerous_tool".to_string()]);
///
/// // read_file is statically allowed
/// let decision = engine.check("read_file", &ApprovalRequirement::Always, None);
/// assert!(matches!(decision, PermissionDecision::Allow { .. }));
///
/// // dangerous_tool is statically denied
/// let decision = engine.check("dangerous_tool", &ApprovalRequirement::Never, None);
/// assert!(matches!(decision, PermissionDecision::Deny { .. }));
/// ```
#[derive(Debug, Clone)]
pub struct PermissionEngine {
    /// The operating mode controlling top-level behavior.
    mode: PermissionMode,
    /// Tool patterns that are always allowed (Layer 2).
    static_allow: Vec<ToolPattern>,
    /// Tool patterns that are always denied (Layer 2).
    static_deny: Vec<ToolPattern>,
    /// Tool patterns granted session-scoped allow via `grant_session_allow` (Layer 3).
    session_allows: Vec<ToolPattern>,
    /// Optional shared session grants for cross-agent session grant sharing.
    shared_session_grants: Option<Arc<RwLock<Vec<ToolPattern>>>>,
}

impl PermissionEngine {
    /// Create a new `PermissionEngine` with the given mode.
    ///
    /// Starts with empty static rules and no session allows.
    pub fn new(mode: PermissionMode) -> Self {
        Self {
            mode,
            static_allow: Vec::new(),
            static_deny: Vec::new(),
            session_allows: Vec::new(),
            shared_session_grants: None,
        }
    }

    /// Builder method: set the static allow list.
    ///
    /// Tools in this list are always permitted without further evaluation.
    /// Each string is parsed via `ToolPattern::parse` with a Bare fallback.
    pub fn with_static_allow(mut self, allow: Vec<String>) -> Self {
        self.static_allow = allow
            .into_iter()
            .map(|s| ToolPattern::parse(&s).unwrap_or_else(|| ToolPattern::Bare(s)))
            .collect();
        self
    }

    /// Builder method: set the static deny list.
    ///
    /// Tools in this list are always denied without further evaluation.
    /// Each string is parsed via `ToolPattern::parse` with a Bare fallback.
    pub fn with_static_deny(mut self, deny: Vec<String>) -> Self {
        self.static_deny = deny
            .into_iter()
            .map(|s| ToolPattern::parse(&s).unwrap_or_else(|| ToolPattern::Bare(s)))
            .collect();
        self
    }

    /// Builder method: apply a merged policy to set static allow and deny lists.
    pub fn with_merged_policy(mut self, policy: MergedPolicy) -> Self {
        self.static_allow = policy.allow;
        self.static_deny = policy.deny;
        self
    }

    /// Builder method: set the shared session grants store for cross-agent sharing.
    pub fn with_shared_session_grants(mut self, store: Arc<RwLock<Vec<ToolPattern>>>) -> Self {
        self.shared_session_grants = Some(store);
        self
    }

    /// Add a tool name to the static allow list.
    pub fn add_static_allow(&mut self, tool_name: impl Into<String>) {
        let s = tool_name.into();
        self.static_allow
            .push(ToolPattern::parse(&s).unwrap_or_else(|| ToolPattern::Bare(s)));
    }

    /// Add a tool name to the static deny list.
    pub fn add_static_deny(&mut self, tool_name: impl Into<String>) {
        let s = tool_name.into();
        self.static_deny
            .push(ToolPattern::parse(&s).unwrap_or_else(|| ToolPattern::Bare(s)));
    }

    /// Get the current permission mode.
    pub fn mode(&self) -> PermissionMode {
        self.mode
    }

    /// Set the permission mode.
    pub fn set_mode(&mut self, mode: PermissionMode) {
        self.mode = mode;
    }

    /// Evaluate the permission decision for a tool call.
    ///
    /// Checks layers in order, short-circuiting at the first definitive decision:
    ///
    /// 1. **Mode check**: Bypass → Allow, DenyAll → Deny
    /// 2. **Static rules**: tool name in deny list → Deny, in allow list → Allow
    /// 3. **Session rules**: tool name in session_allows or shared_session_grants → Allow
    /// 4. **Tool approval requirement**: Never → Allow, Always/Conditional → NeedsApproval
    ///
    /// `tool_input` is optional. When `None`, compound patterns evaluate only
    /// the tool name portion (and never match the arg_glob).
    pub fn check(
        &self,
        tool_name: &str,
        approval_requirement: &ApprovalRequirement,
        tool_input: Option<&serde_json::Value>,
    ) -> PermissionDecision {
        // Layer 1: Mode check
        match self.mode {
            PermissionMode::Bypass => {
                return PermissionDecision::Allow {
                    reason: Some("mode:bypass".to_string()),
                };
            }
            PermissionMode::DenyAll => {
                return PermissionDecision::Deny {
                    message: format!("All tool calls are denied (mode: DenyAll)"),
                    reason: "mode:deny_all".to_string(),
                };
            }
            PermissionMode::Normal => {
                // Continue to next layer
            }
        }

        // Layer 2: Static rules — evaluate with tool_input for compound pattern support
        if self.static_deny.iter().any(|pat| pat.matches(tool_name, tool_input)) {
            return PermissionDecision::Deny {
                message: format!("Tool '{}' is in the static deny list", tool_name),
                reason: "static_deny".to_string(),
            };
        }
        if self.static_allow.iter().any(|pat| pat.matches(tool_name, tool_input)) {
            return PermissionDecision::Allow {
                reason: Some("static_allow".to_string()),
            };
        }

        // Layer 3: Session rules — check local session_allows first, then shared store
        if self.session_allows.iter().any(|pat| pat.matches(tool_name, tool_input)) {
            return PermissionDecision::Allow {
                reason: Some("session_allow".to_string()),
            };
        }

        // Check shared session grants (if present). Uses try_read() since check() is synchronous.
        if let Some(ref shared) = self.shared_session_grants {
            if let Ok(grants) = shared.try_read() {
                if grants.iter().any(|pat| pat.matches(tool_name, tool_input)) {
                    return PermissionDecision::Allow {
                        reason: Some("session_allow".to_string()),
                    };
                }
            }
            // If lock can't be acquired (contention), skip shared store check and fall through to Layer 4
        }

        // Layer 4: Tool approval requirement
        match approval_requirement {
            ApprovalRequirement::Never => PermissionDecision::Allow {
                reason: Some("approval_not_required".to_string()),
            },
            ApprovalRequirement::Always => PermissionDecision::NeedsApproval {
                description: format!("Tool '{}' always requires approval", tool_name),
                call_id: String::new(),
                context: "always".to_string(),
            },
            ApprovalRequirement::Conditional(condition) => PermissionDecision::NeedsApproval {
                description: format!(
                    "Tool '{}' requires approval: {}",
                    tool_name, condition
                ),
                call_id: String::new(),
                context: condition.clone(),
            },
        }
    }

    /// Grant a session-scoped allow for a tool.
    ///
    /// After calling this, subsequent `check()` calls for the given tool name
    /// will return `Allow` at Layer 3 (session rules) without reaching Layer 4.
    ///
    /// This is used when the user approves a tool call with "always allow" —
    /// the tool name is stored for the duration of the current run.
    ///
    /// Accepts both plain tool names (exact match) and pattern strings
    /// (e.g., `Bash(npm*)`, `fs_*`).
    ///
    /// When `shared_session_grants` is present, also writes to the shared store
    /// so all agents in the tree see the grant.
    pub fn grant_session_allow(&mut self, tool_name: &str) {
        let pattern = ToolPattern::parse(tool_name)
            .unwrap_or_else(|| ToolPattern::Bare(tool_name.to_string()));

        // Write to shared session grants store (if present) so all agents see it
        if let Some(ref shared) = self.shared_session_grants {
            if let Ok(mut grants) = shared.try_write() {
                if !grants.iter().any(|p| p == &pattern) {
                    grants.push(pattern.clone());
                }
            }
        }

        // Always push to local session_allows for immediate availability
        if !self.session_allows.iter().any(|p| p == &pattern) {
            self.session_allows.push(pattern);
        }
    }

    /// Check if a tool call is covered by any session grant.
    ///
    /// Checks local session_allows patterns against the given tool name and optional input.
    pub fn has_session_allow(&self, tool_name: &str, tool_input: Option<&serde_json::Value>) -> bool {
        self.session_allows.iter().any(|pat| pat.matches(tool_name, tool_input))
    }

    /// Clear all session-scoped allow rules.
    pub fn clear_session_allows(&mut self) {
        self.session_allows.clear();
    }

    /// Get the static allow list.
    pub fn static_allow_list(&self) -> Vec<String> {
        self.static_allow.iter().map(|p| p.to_string()).collect()
    }

    /// Get the static deny list.
    pub fn static_deny_list(&self) -> Vec<String> {
        self.static_deny.iter().map(|p| p.to_string()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Property-based tests using proptest
    mod property_tests {
        use super::*;
        use proptest::prelude::*;
        use proptest::collection::vec as prop_vec;

        /// **Validates: Requirements 12.3, 12.4, 12.5, 12.6, 12.7**

        /// Strategy for generating valid tool names (non-empty alphanumeric + underscore)
        fn tool_name_strategy() -> impl Strategy<Value = String> {
            "[a-z][a-z0-9_]{0,19}".prop_map(|s| s)
        }

        /// Strategy for generating an ApprovalRequirement
        fn approval_requirement_strategy() -> impl Strategy<Value = ApprovalRequirement> {
            prop_oneof![
                Just(ApprovalRequirement::Never),
                Just(ApprovalRequirement::Always),
                "[a-z ]{1,20}".prop_map(|s| ApprovalRequirement::Conditional(s)),
            ]
        }

        proptest! {
            /// Property: If a tool is in static_deny → always Deny (regardless of approval requirement)
            #[test]
            fn static_deny_always_denies(
                tool_name in tool_name_strategy(),
                other_tools in prop_vec(tool_name_strategy(), 0..5),
                approval in approval_requirement_strategy(),
            ) {
                let mut deny_list = other_tools;
                deny_list.push(tool_name.clone());

                let engine = PermissionEngine::new(PermissionMode::Normal)
                    .with_static_deny(deny_list);

                let decision = engine.check(&tool_name, &approval, None);
                match decision {
                    PermissionDecision::Deny { reason, .. } => {
                        prop_assert_eq!(reason, "static_deny");
                    }
                    other => {
                        prop_assert!(false, "Expected Deny for tool in static_deny, got {:?}", other);
                    }
                }
            }

            /// Property: If a tool is in static_allow (and NOT in static_deny) → always Allow
            #[test]
            fn static_allow_always_allows(
                tool_name in tool_name_strategy(),
                other_tools in prop_vec(tool_name_strategy(), 0..5),
                approval in approval_requirement_strategy(),
            ) {
                let mut allow_list = other_tools;
                allow_list.push(tool_name.clone());

                // Ensure the tool is NOT in the deny list
                let engine = PermissionEngine::new(PermissionMode::Normal)
                    .with_static_allow(allow_list)
                    .with_static_deny(vec![]);

                let decision = engine.check(&tool_name, &approval, None);
                match decision {
                    PermissionDecision::Allow { reason } => {
                        prop_assert_eq!(reason, Some("static_allow".to_string()));
                    }
                    other => {
                        prop_assert!(false, "Expected Allow for tool in static_allow, got {:?}", other);
                    }
                }
            }

            /// Property: static_deny takes precedence over static_allow if a tool is in both
            #[test]
            fn static_deny_takes_precedence_over_static_allow(
                tool_name in tool_name_strategy(),
                approval in approval_requirement_strategy(),
            ) {
                let engine = PermissionEngine::new(PermissionMode::Normal)
                    .with_static_deny(vec![tool_name.clone()])
                    .with_static_allow(vec![tool_name.clone()]);

                let decision = engine.check(&tool_name, &approval, None);
                match decision {
                    PermissionDecision::Deny { reason, .. } => {
                        prop_assert_eq!(reason, "static_deny");
                    }
                    other => {
                        prop_assert!(false, "Expected Deny when tool is in both lists, got {:?}", other);
                    }
                }
            }

            /// Property: Bypass mode → always Allow (regardless of any rules)
            #[test]
            fn bypass_mode_always_allows(
                tool_name in tool_name_strategy(),
                deny_list in prop_vec(tool_name_strategy(), 0..5),
                allow_list in prop_vec(tool_name_strategy(), 0..5),
                approval in approval_requirement_strategy(),
            ) {
                let engine = PermissionEngine::new(PermissionMode::Bypass)
                    .with_static_deny(deny_list)
                    .with_static_allow(allow_list);

                let decision = engine.check(&tool_name, &approval, None);
                match decision {
                    PermissionDecision::Allow { reason } => {
                        prop_assert_eq!(reason, Some("mode:bypass".to_string()));
                    }
                    other => {
                        prop_assert!(false, "Expected Allow in Bypass mode, got {:?}", other);
                    }
                }
            }

            /// Property: DenyAll mode → always Deny (regardless of any rules)
            #[test]
            fn deny_all_mode_always_denies(
                tool_name in tool_name_strategy(),
                deny_list in prop_vec(tool_name_strategy(), 0..5),
                allow_list in prop_vec(tool_name_strategy(), 0..5),
                approval in approval_requirement_strategy(),
            ) {
                let engine = PermissionEngine::new(PermissionMode::DenyAll)
                    .with_static_deny(deny_list)
                    .with_static_allow(allow_list);

                let decision = engine.check(&tool_name, &approval, None);
                match decision {
                    PermissionDecision::Deny { reason, .. } => {
                        prop_assert_eq!(reason, "mode:deny_all");
                    }
                    other => {
                        prop_assert!(false, "Expected Deny in DenyAll mode, got {:?}", other);
                    }
                }
            }

            /// Property: session_allow works for tools not in static lists
            #[test]
            fn session_allow_works_for_non_static_tools(
                tool_name in tool_name_strategy(),
                approval in approval_requirement_strategy(),
            ) {
                let mut engine = PermissionEngine::new(PermissionMode::Normal)
                    .with_static_deny(vec![])
                    .with_static_allow(vec![]);

                engine.grant_session_allow(&tool_name);

                let decision = engine.check(&tool_name, &approval, None);
                match decision {
                    PermissionDecision::Allow { reason } => {
                        prop_assert_eq!(reason, Some("session_allow".to_string()));
                    }
                    other => {
                        prop_assert!(false, "Expected Allow for session-allowed tool, got {:?}", other);
                    }
                }
            }

            /// Property: session_allow does NOT override static_deny (Layer 2 precedes Layer 3)
            #[test]
            fn session_allow_does_not_override_static_deny(
                tool_name in tool_name_strategy(),
                approval in approval_requirement_strategy(),
            ) {
                let mut engine = PermissionEngine::new(PermissionMode::Normal)
                    .with_static_deny(vec![tool_name.clone()]);

                engine.grant_session_allow(&tool_name);

                let decision = engine.check(&tool_name, &approval, None);
                match decision {
                    PermissionDecision::Deny { reason, .. } => {
                        prop_assert_eq!(reason, "static_deny");
                    }
                    other => {
                        prop_assert!(false, "Expected Deny (static_deny overrides session_allow), got {:?}", other);
                    }
                }
            }

            // ===================================================================
            // Property 9: First-Match Rule Ordering
            // **Validates: Requirements 5.5, 6.2, 6.4, 6.5, 7.4, 7.6**
            // ===================================================================

            /// Property 9: The first matching rule in an ordered list determines the decision.
            ///
            /// When the static_allow list contains a specific tool followed by a glob
            /// that would also match, the decision should come from the first match.
            /// Conversely, when tools are ordered so a glob appears first, that glob
            /// determines the decision for subsequent specific entries too.
            ///
            /// **Validates: Requirements 5.5, 6.2, 6.4, 6.5, 7.4, 7.6**
            #[test]
            fn first_match_rule_ordering(
                prefix in "[a-z]{2,6}",
                suffix in "[a-z]{1,6}",
            ) {
                let specific_tool = format!("{}_{}", prefix, suffix);
                let glob_pattern = format!("{}_*", prefix);

                // Case 1: If static_deny has the glob first, it denies the specific tool
                let engine = PermissionEngine::new(PermissionMode::Normal)
                    .with_static_deny(vec![glob_pattern.clone()])
                    .with_static_allow(vec![specific_tool.clone()]);

                let decision = engine.check(&specific_tool, &ApprovalRequirement::Always, None);
                // static_deny is checked before static_allow (Layer 2 ordering: deny first)
                match decision {
                    PermissionDecision::Deny { reason, .. } => {
                        prop_assert_eq!(reason, "static_deny");
                    }
                    other => {
                        prop_assert!(false, "Expected Deny from glob in deny list, got {:?}", other);
                    }
                }

                // Case 2: If static_allow has the glob and deny list is empty,
                // both the specific tool and any matching tool are allowed
                let engine2 = PermissionEngine::new(PermissionMode::Normal)
                    .with_static_allow(vec![glob_pattern.clone()]);

                let another_tool = format!("{}_{}", prefix, "xyz");
                let decision2 = engine2.check(&another_tool, &ApprovalRequirement::Always, None);
                match decision2 {
                    PermissionDecision::Allow { reason } => {
                        prop_assert_eq!(reason, Some("static_allow".to_string()));
                    }
                    other => {
                        prop_assert!(false, "Expected Allow from glob in allow list, got {:?}", other);
                    }
                }
            }

            // ===================================================================
            // Property 10: Pattern-Based Session Grants
            // **Validates: Requirements 5.5, 6.2, 6.4, 6.5, 7.4, 7.6**
            // ===================================================================

            /// Property 10: Pattern-based session grants match subsequent tool calls correctly.
            ///
            /// When a glob pattern is granted as a session allow, any tool whose name
            /// matches that pattern should be allowed at Layer 3. Tools that don't
            /// match the pattern should not be affected.
            ///
            /// **Validates: Requirements 5.5, 6.2, 6.4, 6.5, 7.4, 7.6**
            #[test]
            fn pattern_based_session_grants(
                prefix in "[a-z]{2,6}",
                matching_suffix in "[a-z]{1,6}",
                non_matching_prefix in "[0-9]{2,6}",
            ) {
                let glob_pattern = format!("{}_*", prefix);
                let matching_tool = format!("{}_{}", prefix, matching_suffix);
                let non_matching_tool = format!("{}_tool", non_matching_prefix);

                let mut engine = PermissionEngine::new(PermissionMode::Normal);
                engine.grant_session_allow(&glob_pattern);

                // Matching tool should be allowed via session grant
                let decision = engine.check(&matching_tool, &ApprovalRequirement::Always, None);
                match decision {
                    PermissionDecision::Allow { reason } => {
                        prop_assert_eq!(reason, Some("session_allow".to_string()));
                    }
                    other => {
                        prop_assert!(false,
                            "Expected Allow for tool '{}' matching session grant pattern '{}', got {:?}",
                            matching_tool, glob_pattern, other
                        );
                    }
                }

                // Non-matching tool should NOT be allowed
                let decision2 = engine.check(&non_matching_tool, &ApprovalRequirement::Always, None);
                match decision2 {
                    PermissionDecision::NeedsApproval { .. } => {
                        // Expected: falls through to Layer 4
                    }
                    other => {
                        prop_assert!(false,
                            "Expected NeedsApproval for tool '{}' NOT matching pattern '{}', got {:?}",
                            non_matching_tool, glob_pattern, other
                        );
                    }
                }
            }
        }

        // ===================================================================
        // Property 11: Shared Session Grant Propagation
        // **Validates: Requirements 5.5, 6.2, 6.4, 6.5, 7.4, 7.6**
        // ===================================================================

        /// Property 11: Shared session grant store — a grant written by one engine is visible
        /// to another engine sharing the same Arc.
        ///
        /// When two PermissionEngine instances share the same `Arc<RwLock<Vec<ToolPattern>>>`,
        /// a session grant written by one engine becomes visible to the other engine's
        /// `check()` method via the shared store.
        ///
        /// **Validates: Requirements 5.5, 6.2, 6.4, 6.5, 7.4, 7.6**
        #[test]
        fn shared_session_grant_propagation() {
            let shared_store: Arc<RwLock<Vec<ToolPattern>>> = Arc::new(RwLock::new(Vec::new()));

            let mut engine_a = PermissionEngine::new(PermissionMode::Normal)
                .with_shared_session_grants(shared_store.clone());
            let engine_b = PermissionEngine::new(PermissionMode::Normal)
                .with_shared_session_grants(shared_store.clone());

            // Before grant: engine_b should not allow the tool
            let decision = engine_b.check("shell", &ApprovalRequirement::Always, None);
            assert!(
                matches!(decision, PermissionDecision::NeedsApproval { .. }),
                "Expected NeedsApproval before grant, got {:?}",
                decision
            );

            // Engine A grants a session allow
            engine_a.grant_session_allow("shell");

            // After grant: engine_b should see the grant via shared store
            let decision = engine_b.check("shell", &ApprovalRequirement::Always, None);
            assert!(
                matches!(decision, PermissionDecision::Allow { .. }),
                "Expected Allow after shared grant, got {:?}",
                decision
            );
        }

        /// Property 11b: Shared session grant propagation with glob patterns.
        ///
        /// A glob pattern granted on one engine should match tool calls on the other.
        #[test]
        fn shared_session_grant_propagation_with_glob() {
            let shared_store: Arc<RwLock<Vec<ToolPattern>>> = Arc::new(RwLock::new(Vec::new()));

            let mut engine_a = PermissionEngine::new(PermissionMode::Normal)
                .with_shared_session_grants(shared_store.clone());
            let engine_b = PermissionEngine::new(PermissionMode::Normal)
                .with_shared_session_grants(shared_store.clone());

            // Engine A grants a pattern-based session allow
            engine_a.grant_session_allow("fs_*");

            // Engine B should see matching tools as allowed
            let decision = engine_b.check("fs_read", &ApprovalRequirement::Always, None);
            assert!(
                matches!(decision, PermissionDecision::Allow { .. }),
                "Expected Allow for 'fs_read' via shared glob grant 'fs_*', got {:?}",
                decision
            );

            let decision = engine_b.check("fs_write", &ApprovalRequirement::Always, None);
            assert!(
                matches!(decision, PermissionDecision::Allow { .. }),
                "Expected Allow for 'fs_write' via shared glob grant 'fs_*', got {:?}",
                decision
            );

            // Non-matching tool should still require approval
            let decision = engine_b.check("bash", &ApprovalRequirement::Always, None);
            assert!(
                matches!(decision, PermissionDecision::NeedsApproval { .. }),
                "Expected NeedsApproval for 'bash' (not matching 'fs_*'), got {:?}",
                decision
            );
        }

        /// Property 11c: Shared session grant with compound pattern.
        ///
        /// A compound pattern granted on one engine should match tool calls with
        /// matching arguments on the other engine.
        #[test]
        fn shared_session_grant_propagation_compound() {
            let shared_store: Arc<RwLock<Vec<ToolPattern>>> = Arc::new(RwLock::new(Vec::new()));

            let mut engine_a = PermissionEngine::new(PermissionMode::Normal)
                .with_shared_session_grants(shared_store.clone());
            let engine_b = PermissionEngine::new(PermissionMode::Normal)
                .with_shared_session_grants(shared_store.clone());

            // Engine A grants a compound pattern
            engine_a.grant_session_allow("Bash(npm*)");

            // Engine B should allow matching calls
            let input = serde_json::json!({"command": "npm install"});
            let decision = engine_b.check("Bash", &ApprovalRequirement::Always, Some(&input));
            assert!(
                matches!(decision, PermissionDecision::Allow { .. }),
                "Expected Allow for 'Bash' with 'npm install' via shared compound grant, got {:?}",
                decision
            );

            // Non-matching argument should not be allowed
            let input = serde_json::json!({"command": "cargo build"});
            let decision = engine_b.check("Bash", &ApprovalRequirement::Always, Some(&input));
            assert!(
                matches!(decision, PermissionDecision::NeedsApproval { .. }),
                "Expected NeedsApproval for 'Bash' with 'cargo build', got {:?}",
                decision
            );
        }
    }

    #[test]
    fn permission_decision_allow_variant() {
        let decision = PermissionDecision::Allow {
            reason: Some("test".to_string()),
        };
        let cloned = decision.clone();
        assert_eq!(decision, cloned);
        let debug = format!("{:?}", decision);
        assert!(debug.contains("Allow"));
    }

    #[test]
    fn permission_decision_deny_variant() {
        let decision = PermissionDecision::Deny {
            message: "not allowed".to_string(),
            reason: "policy".to_string(),
        };
        let cloned = decision.clone();
        assert_eq!(decision, cloned);
        let debug = format!("{:?}", decision);
        assert!(debug.contains("Deny"));
        assert!(debug.contains("not allowed"));
    }

    #[test]
    fn permission_decision_needs_approval_variant() {
        let decision = PermissionDecision::NeedsApproval {
            description: "needs user ok".to_string(),
            call_id: "call-1".to_string(),
            context: "dangerous op".to_string(),
        };
        let cloned = decision.clone();
        assert_eq!(decision, cloned);
        let debug = format!("{:?}", decision);
        assert!(debug.contains("NeedsApproval"));
    }

    #[test]
    fn permission_mode_debug_clone_copy_eq() {
        let bypass = PermissionMode::Bypass;
        let normal = PermissionMode::Normal;
        let deny_all = PermissionMode::DenyAll;

        assert_eq!(bypass, PermissionMode::Bypass);
        assert_ne!(bypass, normal);
        assert_ne!(normal, deny_all);

        // Copy
        let copy = bypass;
        assert_eq!(bypass, copy);

        // Debug
        assert!(format!("{:?}", bypass).contains("Bypass"));
        assert!(format!("{:?}", normal).contains("Normal"));
        assert!(format!("{:?}", deny_all).contains("DenyAll"));
    }

    #[test]
    fn engine_new_defaults() {
        let engine = PermissionEngine::new(PermissionMode::Normal);
        assert_eq!(engine.mode(), PermissionMode::Normal);
        assert!(engine.static_allow_list().is_empty());
        assert!(engine.static_deny_list().is_empty());
        assert!(!engine.has_session_allow("anything", None));
    }

    #[test]
    fn engine_builder_pattern() {
        let engine = PermissionEngine::new(PermissionMode::Normal)
            .with_static_allow(vec!["read_file".to_string(), "glob".to_string()])
            .with_static_deny(vec!["shell".to_string()]);

        assert_eq!(engine.static_allow_list().len(), 2);
        assert_eq!(engine.static_deny_list().len(), 1);
    }

    #[test]
    fn engine_add_static_rules() {
        let mut engine = PermissionEngine::new(PermissionMode::Normal);
        engine.add_static_allow("read_file");
        engine.add_static_deny("shell");

        assert_eq!(engine.static_allow_list(), vec!["read_file".to_string()]);
        assert_eq!(engine.static_deny_list(), vec!["shell".to_string()]);
    }

    #[test]
    fn layer1_bypass_mode_allows_all() {
        let engine = PermissionEngine::new(PermissionMode::Bypass);

        // Even tools with Always approval requirement are allowed in Bypass mode
        let decision = engine.check("dangerous_tool", &ApprovalRequirement::Always, None);
        assert!(matches!(decision, PermissionDecision::Allow { .. }));

        let decision = engine.check("any_tool", &ApprovalRequirement::Never, None);
        assert!(matches!(decision, PermissionDecision::Allow { .. }));
    }

    #[test]
    fn layer1_deny_all_mode_denies_all() {
        let engine = PermissionEngine::new(PermissionMode::DenyAll);

        let decision = engine.check("safe_tool", &ApprovalRequirement::Never, None);
        assert!(matches!(decision, PermissionDecision::Deny { .. }));

        let decision = engine.check("any_tool", &ApprovalRequirement::Always, None);
        assert!(matches!(decision, PermissionDecision::Deny { .. }));
    }

    #[test]
    fn layer1_bypass_overrides_static_deny() {
        // In Bypass mode, even statically denied tools are allowed
        let engine = PermissionEngine::new(PermissionMode::Bypass)
            .with_static_deny(vec!["shell".to_string()]);

        let decision = engine.check("shell", &ApprovalRequirement::Always, None);
        assert!(matches!(decision, PermissionDecision::Allow { .. }));
    }

    #[test]
    fn layer1_deny_all_overrides_static_allow() {
        // In DenyAll mode, even statically allowed tools are denied
        let engine = PermissionEngine::new(PermissionMode::DenyAll)
            .with_static_allow(vec!["read_file".to_string()]);

        let decision = engine.check("read_file", &ApprovalRequirement::Never, None);
        assert!(matches!(decision, PermissionDecision::Deny { .. }));
    }

    #[test]
    fn layer2_static_deny_short_circuits() {
        let engine = PermissionEngine::new(PermissionMode::Normal)
            .with_static_deny(vec!["shell".to_string()]);

        let decision = engine.check("shell", &ApprovalRequirement::Never, None);
        match decision {
            PermissionDecision::Deny { reason, .. } => {
                assert_eq!(reason, "static_deny");
            }
            _ => panic!("expected Deny for statically denied tool"),
        }
    }

    #[test]
    fn layer2_static_allow_short_circuits() {
        let engine = PermissionEngine::new(PermissionMode::Normal)
            .with_static_allow(vec!["read_file".to_string()]);

        // Even if tool would normally need approval, static allow wins
        let decision = engine.check("read_file", &ApprovalRequirement::Always, None);
        match decision {
            PermissionDecision::Allow { reason } => {
                assert_eq!(reason, Some("static_allow".to_string()));
            }
            _ => panic!("expected Allow for statically allowed tool"),
        }
    }

    #[test]
    fn layer2_static_deny_takes_precedence_over_static_allow() {
        // If a tool is in both lists, deny takes precedence (checked first)
        let engine = PermissionEngine::new(PermissionMode::Normal)
            .with_static_deny(vec!["ambiguous".to_string()])
            .with_static_allow(vec!["ambiguous".to_string()]);

        let decision = engine.check("ambiguous", &ApprovalRequirement::Never, None);
        assert!(matches!(decision, PermissionDecision::Deny { .. }));
    }

    #[test]
    fn layer3_session_allow_short_circuits() {
        let mut engine = PermissionEngine::new(PermissionMode::Normal);
        engine.grant_session_allow("shell");

        // Even though shell would normally need approval, session allow wins
        let decision = engine.check("shell", &ApprovalRequirement::Always, None);
        match decision {
            PermissionDecision::Allow { reason } => {
                assert_eq!(reason, Some("session_allow".to_string()));
            }
            _ => panic!("expected Allow for session-allowed tool"),
        }
    }

    #[test]
    fn layer3_session_allow_does_not_override_static_deny() {
        let mut engine = PermissionEngine::new(PermissionMode::Normal)
            .with_static_deny(vec!["shell".to_string()]);
        engine.grant_session_allow("shell");

        // Static deny (Layer 2) takes precedence over session allow (Layer 3)
        let decision = engine.check("shell", &ApprovalRequirement::Never, None);
        assert!(matches!(decision, PermissionDecision::Deny { .. }));
    }

    #[test]
    fn layer4_approval_never_allows() {
        let engine = PermissionEngine::new(PermissionMode::Normal);

        let decision = engine.check("some_tool", &ApprovalRequirement::Never, None);
        match decision {
            PermissionDecision::Allow { reason } => {
                assert_eq!(reason, Some("approval_not_required".to_string()));
            }
            _ => panic!("expected Allow for tool with ApprovalRequirement::Never"),
        }
    }

    #[test]
    fn layer4_approval_always_needs_approval() {
        let engine = PermissionEngine::new(PermissionMode::Normal);

        let decision = engine.check("dangerous_tool", &ApprovalRequirement::Always, None);
        match decision {
            PermissionDecision::NeedsApproval { description, context, .. } => {
                assert!(description.contains("dangerous_tool"));
                assert_eq!(context, "always");
            }
            _ => panic!("expected NeedsApproval for tool with ApprovalRequirement::Always"),
        }
    }

    #[test]
    fn layer4_approval_conditional_needs_approval() {
        let engine = PermissionEngine::new(PermissionMode::Normal);

        let decision = engine.check(
            "shell",
            &ApprovalRequirement::Conditional("writing to /etc".to_string()),
            None,
        );
        match decision {
            PermissionDecision::NeedsApproval { description, context, .. } => {
                assert!(description.contains("shell"));
                assert!(description.contains("writing to /etc"));
                assert_eq!(context, "writing to /etc");
            }
            _ => panic!("expected NeedsApproval for conditional requirement"),
        }
    }

    #[test]
    fn grant_session_allow_persists() {
        let mut engine = PermissionEngine::new(PermissionMode::Normal);

        assert!(!engine.has_session_allow("shell", None));
        engine.grant_session_allow("shell");
        assert!(engine.has_session_allow("shell", None));

        // Multiple grants for different tools
        engine.grant_session_allow("file_write");
        assert!(engine.has_session_allow("shell", None));
        assert!(engine.has_session_allow("file_write", None));
    }

    #[test]
    fn clear_session_allows_removes_all() {
        let mut engine = PermissionEngine::new(PermissionMode::Normal);
        engine.grant_session_allow("shell");
        engine.grant_session_allow("file_write");

        assert!(engine.has_session_allow("shell", None));
        engine.clear_session_allows();
        assert!(!engine.has_session_allow("shell", None));
        assert!(!engine.has_session_allow("file_write", None));
    }

    #[test]
    fn grant_session_allow_idempotent() {
        let mut engine = PermissionEngine::new(PermissionMode::Normal);
        engine.grant_session_allow("shell");
        engine.grant_session_allow("shell"); // duplicate is fine

        assert!(engine.has_session_allow("shell", None));
    }

    #[test]
    fn set_mode_changes_behavior() {
        let mut engine = PermissionEngine::new(PermissionMode::Normal);

        let decision = engine.check("tool", &ApprovalRequirement::Always, None);
        assert!(matches!(decision, PermissionDecision::NeedsApproval { .. }));

        engine.set_mode(PermissionMode::Bypass);
        let decision = engine.check("tool", &ApprovalRequirement::Always, None);
        assert!(matches!(decision, PermissionDecision::Allow { .. }));

        engine.set_mode(PermissionMode::DenyAll);
        let decision = engine.check("tool", &ApprovalRequirement::Never, None);
        assert!(matches!(decision, PermissionDecision::Deny { .. }));
    }

    #[test]
    fn full_pipeline_normal_flow() {
        // Tool not in any static list, no session allow, ApprovalRequirement::Never
        let engine = PermissionEngine::new(PermissionMode::Normal)
            .with_static_allow(vec!["read_file".to_string()])
            .with_static_deny(vec!["rm_rf".to_string()]);

        // Normal tool, no approval needed
        let decision = engine.check("grep", &ApprovalRequirement::Never, None);
        assert!(matches!(decision, PermissionDecision::Allow { .. }));

        // Tool requiring approval
        let decision = engine.check("shell", &ApprovalRequirement::Always, None);
        assert!(matches!(decision, PermissionDecision::NeedsApproval { .. }));
    }

    #[test]
    fn full_pipeline_with_session_allow_after_approval() {
        let mut engine = PermissionEngine::new(PermissionMode::Normal);

        // First call: needs approval
        let decision = engine.check("shell", &ApprovalRequirement::Always, None);
        assert!(matches!(decision, PermissionDecision::NeedsApproval { .. }));

        // User approves with "always allow"
        engine.grant_session_allow("shell");

        // Subsequent calls: allowed via session rule
        let decision = engine.check("shell", &ApprovalRequirement::Always, None);
        assert!(matches!(decision, PermissionDecision::Allow { reason } if reason == Some("session_allow".to_string())));
    }
}

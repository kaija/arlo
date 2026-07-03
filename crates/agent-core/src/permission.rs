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

use std::collections::HashSet;

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
/// let decision = engine.check("read_file", &ApprovalRequirement::Always);
/// assert!(matches!(decision, PermissionDecision::Allow { .. }));
///
/// // dangerous_tool is statically denied
/// let decision = engine.check("dangerous_tool", &ApprovalRequirement::Never);
/// assert!(matches!(decision, PermissionDecision::Deny { .. }));
/// ```
#[derive(Debug, Clone)]
pub struct PermissionEngine {
    /// The operating mode controlling top-level behavior.
    mode: PermissionMode,
    /// Tool names that are always allowed (Layer 2).
    static_allow: Vec<String>,
    /// Tool names that are always denied (Layer 2).
    static_deny: Vec<String>,
    /// Tool names granted session-scoped allow via `grant_session_allow` (Layer 3).
    session_allows: HashSet<String>,
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
            session_allows: HashSet::new(),
        }
    }

    /// Builder method: set the static allow list.
    ///
    /// Tools in this list are always permitted without further evaluation.
    pub fn with_static_allow(mut self, allow: Vec<String>) -> Self {
        self.static_allow = allow;
        self
    }

    /// Builder method: set the static deny list.
    ///
    /// Tools in this list are always denied without further evaluation.
    pub fn with_static_deny(mut self, deny: Vec<String>) -> Self {
        self.static_deny = deny;
        self
    }

    /// Add a tool name to the static allow list.
    pub fn add_static_allow(&mut self, tool_name: impl Into<String>) {
        self.static_allow.push(tool_name.into());
    }

    /// Add a tool name to the static deny list.
    pub fn add_static_deny(&mut self, tool_name: impl Into<String>) {
        self.static_deny.push(tool_name.into());
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
    /// 3. **Session rules**: tool name in session_allows → Allow
    /// 4. **Tool approval requirement**: Never → Allow, Always/Conditional → NeedsApproval
    pub fn check(
        &self,
        tool_name: &str,
        approval_requirement: &ApprovalRequirement,
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

        // Layer 2: Static rules
        if self.static_deny.iter().any(|name| name == tool_name) {
            return PermissionDecision::Deny {
                message: format!("Tool '{}' is in the static deny list", tool_name),
                reason: "static_deny".to_string(),
            };
        }
        if self.static_allow.iter().any(|name| name == tool_name) {
            return PermissionDecision::Allow {
                reason: Some("static_allow".to_string()),
            };
        }

        // Layer 3: Session rules
        if self.session_allows.contains(tool_name) {
            return PermissionDecision::Allow {
                reason: Some("session_allow".to_string()),
            };
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
    pub fn grant_session_allow(&mut self, tool_name: &str) {
        self.session_allows.insert(tool_name.to_string());
    }

    /// Check if a tool has been granted session-level allow.
    pub fn has_session_allow(&self, tool_name: &str) -> bool {
        self.session_allows.contains(tool_name)
    }

    /// Clear all session-scoped allow rules.
    pub fn clear_session_allows(&mut self) {
        self.session_allows.clear();
    }

    /// Get the static allow list.
    pub fn static_allow_list(&self) -> &[String] {
        &self.static_allow
    }

    /// Get the static deny list.
    pub fn static_deny_list(&self) -> &[String] {
        &self.static_deny
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

                let decision = engine.check(&tool_name, &approval);
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

                let decision = engine.check(&tool_name, &approval);
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

                let decision = engine.check(&tool_name, &approval);
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

                let decision = engine.check(&tool_name, &approval);
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

                let decision = engine.check(&tool_name, &approval);
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

                let decision = engine.check(&tool_name, &approval);
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

                let decision = engine.check(&tool_name, &approval);
                match decision {
                    PermissionDecision::Deny { reason, .. } => {
                        prop_assert_eq!(reason, "static_deny");
                    }
                    other => {
                        prop_assert!(false, "Expected Deny (static_deny overrides session_allow), got {:?}", other);
                    }
                }
            }
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
        assert!(!engine.has_session_allow("anything"));
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

        assert_eq!(engine.static_allow_list(), &["read_file".to_string()]);
        assert_eq!(engine.static_deny_list(), &["shell".to_string()]);
    }

    #[test]
    fn layer1_bypass_mode_allows_all() {
        let engine = PermissionEngine::new(PermissionMode::Bypass);

        // Even tools with Always approval requirement are allowed in Bypass mode
        let decision = engine.check("dangerous_tool", &ApprovalRequirement::Always);
        assert!(matches!(decision, PermissionDecision::Allow { .. }));

        let decision = engine.check("any_tool", &ApprovalRequirement::Never);
        assert!(matches!(decision, PermissionDecision::Allow { .. }));
    }

    #[test]
    fn layer1_deny_all_mode_denies_all() {
        let engine = PermissionEngine::new(PermissionMode::DenyAll);

        let decision = engine.check("safe_tool", &ApprovalRequirement::Never);
        assert!(matches!(decision, PermissionDecision::Deny { .. }));

        let decision = engine.check("any_tool", &ApprovalRequirement::Always);
        assert!(matches!(decision, PermissionDecision::Deny { .. }));
    }

    #[test]
    fn layer1_bypass_overrides_static_deny() {
        // In Bypass mode, even statically denied tools are allowed
        let engine = PermissionEngine::new(PermissionMode::Bypass)
            .with_static_deny(vec!["shell".to_string()]);

        let decision = engine.check("shell", &ApprovalRequirement::Always);
        assert!(matches!(decision, PermissionDecision::Allow { .. }));
    }

    #[test]
    fn layer1_deny_all_overrides_static_allow() {
        // In DenyAll mode, even statically allowed tools are denied
        let engine = PermissionEngine::new(PermissionMode::DenyAll)
            .with_static_allow(vec!["read_file".to_string()]);

        let decision = engine.check("read_file", &ApprovalRequirement::Never);
        assert!(matches!(decision, PermissionDecision::Deny { .. }));
    }

    #[test]
    fn layer2_static_deny_short_circuits() {
        let engine = PermissionEngine::new(PermissionMode::Normal)
            .with_static_deny(vec!["shell".to_string()]);

        let decision = engine.check("shell", &ApprovalRequirement::Never);
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
        let decision = engine.check("read_file", &ApprovalRequirement::Always);
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

        let decision = engine.check("ambiguous", &ApprovalRequirement::Never);
        assert!(matches!(decision, PermissionDecision::Deny { .. }));
    }

    #[test]
    fn layer3_session_allow_short_circuits() {
        let mut engine = PermissionEngine::new(PermissionMode::Normal);
        engine.grant_session_allow("shell");

        // Even though shell would normally need approval, session allow wins
        let decision = engine.check("shell", &ApprovalRequirement::Always);
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
        let decision = engine.check("shell", &ApprovalRequirement::Never);
        assert!(matches!(decision, PermissionDecision::Deny { .. }));
    }

    #[test]
    fn layer4_approval_never_allows() {
        let engine = PermissionEngine::new(PermissionMode::Normal);

        let decision = engine.check("some_tool", &ApprovalRequirement::Never);
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

        let decision = engine.check("dangerous_tool", &ApprovalRequirement::Always);
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

        assert!(!engine.has_session_allow("shell"));
        engine.grant_session_allow("shell");
        assert!(engine.has_session_allow("shell"));

        // Multiple grants for different tools
        engine.grant_session_allow("file_write");
        assert!(engine.has_session_allow("shell"));
        assert!(engine.has_session_allow("file_write"));
    }

    #[test]
    fn clear_session_allows_removes_all() {
        let mut engine = PermissionEngine::new(PermissionMode::Normal);
        engine.grant_session_allow("shell");
        engine.grant_session_allow("file_write");

        assert!(engine.has_session_allow("shell"));
        engine.clear_session_allows();
        assert!(!engine.has_session_allow("shell"));
        assert!(!engine.has_session_allow("file_write"));
    }

    #[test]
    fn grant_session_allow_idempotent() {
        let mut engine = PermissionEngine::new(PermissionMode::Normal);
        engine.grant_session_allow("shell");
        engine.grant_session_allow("shell"); // duplicate is fine

        assert!(engine.has_session_allow("shell"));
    }

    #[test]
    fn set_mode_changes_behavior() {
        let mut engine = PermissionEngine::new(PermissionMode::Normal);

        let decision = engine.check("tool", &ApprovalRequirement::Always);
        assert!(matches!(decision, PermissionDecision::NeedsApproval { .. }));

        engine.set_mode(PermissionMode::Bypass);
        let decision = engine.check("tool", &ApprovalRequirement::Always);
        assert!(matches!(decision, PermissionDecision::Allow { .. }));

        engine.set_mode(PermissionMode::DenyAll);
        let decision = engine.check("tool", &ApprovalRequirement::Never);
        assert!(matches!(decision, PermissionDecision::Deny { .. }));
    }

    #[test]
    fn full_pipeline_normal_flow() {
        // Tool not in any static list, no session allow, ApprovalRequirement::Never
        let engine = PermissionEngine::new(PermissionMode::Normal)
            .with_static_allow(vec!["read_file".to_string()])
            .with_static_deny(vec!["rm_rf".to_string()]);

        // Normal tool, no approval needed
        let decision = engine.check("grep", &ApprovalRequirement::Never);
        assert!(matches!(decision, PermissionDecision::Allow { .. }));

        // Tool requiring approval
        let decision = engine.check("shell", &ApprovalRequirement::Always);
        assert!(matches!(decision, PermissionDecision::NeedsApproval { .. }));
    }

    #[test]
    fn full_pipeline_with_session_allow_after_approval() {
        let mut engine = PermissionEngine::new(PermissionMode::Normal);

        // First call: needs approval
        let decision = engine.check("shell", &ApprovalRequirement::Always);
        assert!(matches!(decision, PermissionDecision::NeedsApproval { .. }));

        // User approves with "always allow"
        engine.grant_session_allow("shell");

        // Subsequent calls: allowed via session rule
        let decision = engine.check("shell", &ApprovalRequirement::Always);
        assert!(matches!(decision, PermissionDecision::Allow { reason } if reason == Some("session_allow".to_string())));
    }
}

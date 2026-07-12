import { useState } from "react";
import type { ApprovalDecision, PermissionRequestValue } from "../types";

interface Props {
  approvals: PermissionRequestValue[];
  onRespond: (callId: string, decision: ApprovalDecision, pattern: string | null) => void;
}

function ApprovalCard({ approval, onRespond }: { approval: PermissionRequestValue; onRespond: Props["onRespond"] }) {
  const label = approval.agentName ?? "main agent";
  return (
    <div className="approval-card" data-call-id={approval.callId}>
      <div className="approval-card__agent">{label}</div>
      <div className="approval-card__tool">
        {approval.toolName}: {JSON.stringify(approval.toolInput)}
      </div>
      <div className="approval-card__actions">
        <button type="button" onClick={() => onRespond(approval.callId, "allow_once", null)}>
          Allow once
        </button>
        <button
          type="button"
          onClick={() => onRespond(approval.callId, "allow_always", approval.toolName)}
        >
          Always allow {approval.toolName}
        </button>
        <button
          type="button"
          className="button--danger"
          onClick={() => onRespond(approval.callId, "reject_once", null)}
        >
          Reject once
        </button>
        <button
          type="button"
          className="button--danger"
          onClick={() => onRespond(approval.callId, "reject_always", null)}
        >
          Always reject
        </button>
      </div>
    </div>
  );
}

export function ApprovalQueue({ approvals, onRespond }: Props) {
  const [expanded, setExpanded] = useState(false);

  if (approvals.length === 0) return null;

  const collapse = approvals.length >= 3 && !expanded;

  return (
    <aside className="approval-queue">
      <h2>Approvals ({approvals.length})</h2>
      {collapse ? (
        <button type="button" onClick={() => setExpanded(true)} className="approval-queue__summary">
          {approvals.length} pending approvals ▾
        </button>
      ) : (
        approvals.map((a) => <ApprovalCard key={a.callId} approval={a} onRespond={onRespond} />)
      )}
    </aside>
  );
}

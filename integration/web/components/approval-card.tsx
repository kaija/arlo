"use client";

import { useInterrupt } from "@copilotkit/react-core/v2";
import { useEffect, useState } from "react";

function TimeRemaining({ expiresAt }: { expiresAt?: string }) {
  const [remaining, setRemaining] = useState<string>("");
  const [expired, setExpired] = useState(false);

  useEffect(() => {
    if (!expiresAt) return;

    const update = () => {
      const diff = new Date(expiresAt).getTime() - Date.now();
      if (diff <= 0) {
        setExpired(true);
        setRemaining("Session expired");
        return;
      }
      const minutes = Math.floor(diff / 60000);
      const seconds = Math.floor((diff % 60000) / 1000);
      setRemaining(`${minutes}:${seconds.toString().padStart(2, "0")} remaining`);
    };

    update();
    const id = setInterval(update, 1000);
    return () => clearInterval(id);
  }, [expiresAt]);

  if (!expiresAt) return null;
  return (
    <span
      className={expired ? "badge badge-danger" : "badge badge-neutral"}
      style={{ fontFamily: "var(--font-mono)", fontWeight: 500 }}
    >
      {remaining}
    </span>
  );
}

export function ApprovalCard() {
  useInterrupt({
    render: ({ interrupt, resolve, cancel }) => {
      const metadata = interrupt?.metadata as
        | { toolName?: string; toolInput?: unknown }
        | undefined;
      const toolName = metadata?.toolName as string | undefined;
      const toolInput = metadata?.toolInput;
      const expiresAt = interrupt?.expiresAt;
      const isExpired = expiresAt ? new Date(expiresAt).getTime() <= Date.now() : false;

      // Fallback: if no metadata, show message or reason
      const description =
        toolName && toolInput
          ? `${toolName}(${JSON.stringify(toolInput, null, 2)})`
          : (interrupt?.message ?? interrupt?.reason ?? "Approval required");

      return (
        <div className="approval card card-sm">
          <div className="approval-head">
            <span className="badge badge-warning">
              <span className="badge-dot" />
              Tool approval required
            </span>
            <TimeRemaining expiresAt={expiresAt} />
          </div>

          <pre className="approval-tool">{description}</pre>

          <div className="approval-actions">
            <button
              className="btn btn-sm btn-primary"
              disabled={isExpired}
              onClick={() => resolve({})}
            >
              Allow once
            </button>
            <button
              className="btn btn-sm"
              disabled={isExpired}
              onClick={() =>
                resolve({
                  action: "always_allow",
                  pattern: toolName ? `${toolName}(*)` : "*",
                })
              }
            >
              Always allow
            </button>
            <button
              className="btn btn-sm btn-danger"
              disabled={isExpired}
              onClick={() => cancel()}
            >
              Deny
            </button>
          </div>

          {isExpired && (
            <p className="field-hint field-hint-error" style={{ marginTop: "12px" }}>
              This session has expired. Reload the page to start a new conversation.
            </p>
          )}
        </div>
      );
    },
  });

  // useInterrupt renders inline — this component renders nothing itself
  return null;
}

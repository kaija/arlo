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
    <span style={{ color: expired ? "red" : "gray", fontSize: "0.85em" }}>
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
        <div
          style={{
            border: "1px solid #e0e0e0",
            borderRadius: "8px",
            padding: "16px",
            margin: "8px 0",
            background: "#fff8e1",
          }}
        >
          <div style={{ fontWeight: 600, marginBottom: "8px" }}>
            Tool approval required
          </div>
          <pre
            style={{
              background: "#f5f5f5",
              padding: "8px",
              borderRadius: "4px",
              fontSize: "0.85em",
              overflow: "auto",
              maxHeight: "200px",
            }}
          >
            {description}
          </pre>
          <div
            style={{
              display: "flex",
              gap: "8px",
              marginTop: "12px",
              alignItems: "center",
            }}
          >
            <button
              disabled={isExpired}
              onClick={() => resolve({})}
              style={{
                padding: "6px 14px",
                background: isExpired ? "#ccc" : "#4caf50",
                color: "#fff",
                border: "none",
                borderRadius: "4px",
                cursor: isExpired ? "not-allowed" : "pointer",
              }}
            >
              Allow once
            </button>
            <button
              disabled={isExpired}
              onClick={() =>
                resolve({
                  action: "always_allow",
                  pattern: toolName ? `${toolName}(*)` : "*",
                })
              }
              style={{
                padding: "6px 14px",
                background: isExpired ? "#ccc" : "#2196f3",
                color: "#fff",
                border: "none",
                borderRadius: "4px",
                cursor: isExpired ? "not-allowed" : "pointer",
              }}
            >
              Always allow
            </button>
            <button
              disabled={isExpired}
              onClick={() => cancel()}
              style={{
                padding: "6px 14px",
                background: isExpired ? "#ccc" : "#f44336",
                color: "#fff",
                border: "none",
                borderRadius: "4px",
                cursor: isExpired ? "not-allowed" : "pointer",
              }}
            >
              Deny
            </button>
            <TimeRemaining expiresAt={expiresAt} />
          </div>
          {isExpired && (
            <p style={{ color: "red", marginTop: "8px", fontSize: "0.9em" }}>
              This session has expired. Please start a new conversation.
            </p>
          )}
        </div>
      );
    },
  });

  // useInterrupt renders inline — this component renders nothing itself
  return null;
}

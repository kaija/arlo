import { useEffect } from "react";
import type { ToastEntry } from "../useAgentSession";

interface Props {
  toasts: ToastEntry[];
  onDismiss: (id: string) => void;
}

function Toast({ toast, onDismiss }: { toast: ToastEntry; onDismiss: (id: string) => void }) {
  useEffect(() => {
    const timer = setTimeout(() => onDismiss(toast.id), 6000);
    return () => clearTimeout(timer);
  }, [toast.id, onDismiss]);

  return (
    <div className={`toast toast--${toast.kind}`}>
      {toast.kind === "success" ? "✓" : "✗"} {toast.text}
    </div>
  );
}

export function Toasts({ toasts, onDismiss }: Props) {
  if (toasts.length === 0) return null;
  return (
    <div className="toast-stack">
      {toasts.map((t) => (
        <Toast key={t.id} toast={t} onDismiss={onDismiss} />
      ))}
    </div>
  );
}

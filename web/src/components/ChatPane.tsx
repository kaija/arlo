import { useState } from "react";
import type { TimelineItem } from "../useAgentSession";

interface Props {
  timeline: TimelineItem[];
  runActive: boolean;
  onSend: (text: string) => void;
  onAbort: () => void;
}

function ToolCard({ item }: { item: Extract<TimelineItem, { kind: "tool" }> }) {
  const icon = item.status === "pending" ? "▸" : item.status === "error" ? "✗" : "✓";
  return (
    <div className={`tool-card tool-card--${item.status}`} data-tool-call-id={item.id}>
      <div className="tool-card__header">
        {icon} {item.name} {item.status === "pending" && <span className="tool-card__badge">awaiting</span>}
      </div>
      {item.output && <pre className="tool-card__output">{item.output}</pre>}
    </div>
  );
}

export function ChatPane({ timeline, runActive, onSend, onAbort }: Props) {
  const [draft, setDraft] = useState("");

  function submit() {
    const text = draft.trim();
    if (!text || runActive) return;
    onSend(text);
    setDraft("");
  }

  return (
    <div className="chat-pane">
      <div className="chat-pane__timeline">
        {timeline.map((item) =>
          item.kind === "text" ? (
            <div key={item.id} className={`bubble bubble--${item.role}`}>
              {item.text || (item.role === "assistant" ? "…" : "")}
            </div>
          ) : (
            <ToolCard key={item.id} item={item} />
          )
        )}
      </div>
      <div className="chat-pane__input">
        <textarea
          value={draft}
          onChange={(e) => setDraft(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter" && !e.shiftKey) {
              e.preventDefault();
              submit();
            }
          }}
          placeholder={runActive ? "Agent is working…" : "Message arlo…"}
          disabled={runActive}
        />
        {runActive ? (
          <button type="button" onClick={onAbort} className="button button--danger">
            Abort
          </button>
        ) : (
          <button type="button" onClick={submit} disabled={!draft.trim()}>
            Send
          </button>
        )}
      </div>
    </div>
  );
}

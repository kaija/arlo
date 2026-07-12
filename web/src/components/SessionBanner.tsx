import type { ConnectionState } from "../useAgentSession";

interface Props {
  connection: ConnectionState;
  onReconnect: () => void;
}

export function SessionBanner({ connection, onReconnect }: Props) {
  if (connection === "connecting" || connection === "open") return null;

  const message =
    connection === "closed_takeover"
      ? "Session moved to another tab."
      : "Connection lost.";

  return (
    <div className="session-banner">
      <p>{message}</p>
      <button type="button" onClick={onReconnect}>
        Reconnect here
      </button>
    </div>
  );
}

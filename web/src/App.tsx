import { useEffect } from "react";
import { useAgentSession } from "./useAgentSession";
import { ChatPane } from "./components/ChatPane";
import { ApprovalQueue } from "./components/ApprovalQueue";
import { Sidebar } from "./components/Sidebar";
import { Toasts } from "./components/Toasts";
import { SessionBanner } from "./components/SessionBanner";

export default function App() {
  const session = useAgentSession();

  useEffect(() => {
    const armed = session.approvals.length > 0 || session.runActive;
    if (!armed) return;
    const handler = (e: BeforeUnloadEvent) => {
      e.preventDefault();
      e.returnValue = "";
    };
    window.addEventListener("beforeunload", handler);
    return () => window.removeEventListener("beforeunload", handler);
  }, [session.approvals.length, session.runActive]);

  if (session.connection === "closed_takeover" || session.connection === "closed_error") {
    return (
      <div className="app app--closed">
        <SessionBanner connection={session.connection} onReconnect={session.reconnect} />
      </div>
    );
  }

  return (
    <div className="app">
      <main className="app__main">
        {session.interruptedNotice && (
          <div className="interrupted-notice">
            ⚠ Previous run was interrupted (opened in another tab)
          </div>
        )}
        <ChatPane
          timeline={session.timeline}
          runActive={session.runActive}
          onSend={session.sendUserMessage}
          onAbort={session.sendAbort}
        />
      </main>
      <ApprovalQueue approvals={session.approvals} onRespond={session.sendApprovalResponse} />
      <Sidebar tasks={session.tasks} todos={session.todos} />
      <Toasts toasts={session.toasts} onDismiss={session.dismissToast} />
    </div>
  );
}

import { useEffect } from "react";
import { useAgentSession } from "./useAgentSession";
import { ChatPane } from "./components/ChatPane";
import { ApprovalQueue } from "./components/ApprovalQueue";
import { Sidebar } from "./components/Sidebar";
import { Toasts } from "./components/Toasts";

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

  return (
    <div className="app">
      <main className="app__main">
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

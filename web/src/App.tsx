import { useAgentSession } from "./useAgentSession";
import { ChatPane } from "./components/ChatPane";

export default function App() {
  const session = useAgentSession();

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
    </div>
  );
}

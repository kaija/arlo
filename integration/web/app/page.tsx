"use client";

import { CopilotChat } from "@copilotkit/react-ui";
import { ApprovalCard } from "../components/approval-card";

export default function Home() {
  return (
    <main style={{ height: "100vh", display: "flex", flexDirection: "column" }}>
      <ApprovalCard />
      <div style={{ flex: 1, overflow: "hidden" }}>
        <CopilotChat
          labels={{
            title: "Arlo",
            initial: "Hi! I'm Arlo. How can I help you today?",
          }}
        />
      </div>
    </main>
  );
}

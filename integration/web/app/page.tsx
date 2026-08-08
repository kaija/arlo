"use client";

import { CopilotChat } from "@copilotkit/react-ui";
import { ApprovalCard } from "../components/approval-card";

function ArloMark() {
  return (
    <svg width="28" height="28" viewBox="0 0 28 28" fill="none" aria-hidden="true">
      <rect width="28" height="28" rx="7" fill="#5856D6" />
      <path
        d="M8 20L14 8L20 20"
        stroke="white"
        strokeWidth="2.5"
        strokeLinecap="round"
        strokeLinejoin="round"
      />
      <path d="M10.6 15.4H17.4" stroke="white" strokeWidth="2.5" strokeLinecap="round" />
    </svg>
  );
}

export default function Home() {
  return (
    <div className="chat-shell">
      <header className="chat-nav">
        <div className="chat-nav-inner">
          <div className="chat-brand">
            <ArloMark />
            Arlo
            <span>local-first agent</span>
          </div>
          <button className="btn btn-sm" onClick={() => location.reload()}>
            New chat
          </button>
        </div>
      </header>

      <main className="chat-body">
        <ApprovalCard />
        <CopilotChat
          labels={{
            title: "Arlo",
            initial:
              "Arlo runs on your machine. Ask for something — you approve every tool it wants to run.",
            placeholder: "Ask Arlo to do something",
          }}
        />
      </main>

      <p className="chat-note">
        Nothing is stored — reloading starts a new conversation.
      </p>
    </div>
  );
}

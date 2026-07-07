# Claude Code Agentic Loop 架構分析

本文件分析 Claude Code 的核心 Agent Loop 運作流程，涵蓋：計畫模式 (Plan)、工具調用 (Tool Execution)、子代理 (Sub-Agent)、人機互動 (HITL)、以及自動停止判斷邏輯。

---

## 1. 高層架構總覽

```
┌─────────────────────────────────────────────────────────────────┐
│                        User / IDE / SDK                          │
└───────────────────────────────┬─────────────────────────────────┘
                                │ prompt
                                ▼
┌─────────────────────────────────────────────────────────────────┐
│                     QueryEngine.submitMessage()                   │
│  ┌───────────────────────────────────────────────────────────┐  │
│  │ 1. 組裝 System Prompt (含 CLAUDE.md, skills, plugins)      │  │
│  │ 2. 處理使用者輸入 (slash commands, attachments)             │  │
│  │ 3. 建立 ToolUseContext (權限、model、abort controller)     │  │
│  │ 4. 呼叫 query() generator — 進入 Main Loop                │  │
│  └───────────────────────────────────────────────────────────┘  │
└───────────────────────────────┬─────────────────────────────────┘
                                │
                                ▼
┌─────────────────────────────────────────────────────────────────┐
│                  query() → queryLoop()                            │
│                  *** 核心 Agent Loop ***                          │
│                  (詳見 Section 2)                                 │
└─────────────────────────────────────────────────────────────────┘
```

---

## 2. 核心 Agent Loop (`queryLoop`)

這是整個系統的心臟。位於 `src/query.ts`，是一個 `while(true)` 無限迴圈，每一輪 (iteration) 代表一次「模型推理 + 工具執行」的回合。

```
┌─────────────────────────────────────────────────────────────────────────┐
│                         queryLoop (while true)                           │
│                                                                         │
│  ┏━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━┓  │
│  ┃ Phase 0: Context Preparation                                      ┃  │
│  ┃  ├─ snipCompact (歷史裁剪)                                        ┃  │
│  ┃  ├─ microcompact (工具結果壓縮)                                    ┃  │
│  ┃  ├─ contextCollapse (上下文摺疊)                                   ┃  │
│  ┃  ├─ autoCompact (自動摘要壓縮)                                     ┃  │
│  ┃  └─ blocking limit check (token 上限檢查)                         ┃  │
│  ┗━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━┛  │
│                                    │                                     │
│                                    ▼                                     │
│  ┏━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━┓  │
│  ┃ Phase 1: Model Inference (Streaming)                              ┃  │
│  ┃  ├─ 呼叫 Anthropic API (deps.callModel)                          ┃  │
│  ┃  ├─ 逐 token 串流接收                                             ┃  │
│  ┃  ├─ 收集 assistant message + tool_use blocks                      ┃  │
│  ┃  ├─ StreamingToolExecutor: 串流中即開始執行工具 (optimistic)       ┃  │
│  ┃  └─ 偵測 needsFollowUp = true (有 tool_use blocks)               ┃  │
│  ┗━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━┛  │
│                                    │                                     │
│                                    ▼                                     │
│                     ┌──────────────────────────────┐                     │
│                     │   needsFollowUp == true ?    │                     │
│                     └──────────┬─────────┬─────────┘                     │
│                        YES     │         │  NO                           │
│                                │         │                               │
│              ┌─────────────────┘         └──────────────┐                │
│              ▼                                          ▼                │
│  ┏━━━━━━━━━━━━━━━━━━━━━━━━━┓          ┏━━━━━━━━━━━━━━━━━━━━━━━━━━━┓   │
│  ┃ Phase 2: Tool Execution  ┃          ┃ Phase 3: Stop Decision    ┃   │
│  ┃ (詳見 Section 3)         ┃          ┃ (詳見 Section 6)          ┃   │
│  ┗━━━━━━━━━━━━━━━━━━━━━━━━━┛          ┗━━━━━━━━━━━━━━━━━━━━━━━━━━━┛   │
│              │                                          │                │
│              ▼                                          ▼                │
│  ┌─────────────────────┐                   ┌───────────────────────┐    │
│  │ Attach context:     │                   │ return Terminal:       │    │
│  │ memory, skills,     │                   │  - 'completed'        │    │
│  │ file changes,       │                   │  - 'stop_hook_blocked'│    │
│  │ queued commands     │                   │  - 'max_turns'        │    │
│  └─────────┬───────────┘                   │  - 'prompt_too_long'  │    │
│            │                               │  - etc.               │    │
│            ▼                               └───────────────────────┘    │
│  ┌─────────────────────┐                                                │
│  │ state = next_turn   │ ◄── continue (回到 while loop 頂部)            │
│  └─────────────────────┘                                                │
│                                                                         │
└─────────────────────────────────────────────────────────────────────────┘
```

---

## 3. 工具執行流程 (Tool Execution)

當模型回應中包含 `tool_use` blocks 時，系統進入工具執行階段。

```
┌───────────────────────────────────────────────────────────────┐
│                     Tool Execution Pipeline                     │
│                                                                 │
│  tool_use blocks (from model response)                          │
│         │                                                       │
│         ▼                                                       │
│  ┌─────────────────────────────────────────────────┐           │
│  │ Partition (toolOrchestration.ts)                 │           │
│  │                                                 │           │
│  │  tool.isConcurrencySafe(input) ?                │           │
│  │     YES → 加入 concurrent batch                  │           │
│  │     NO  → 獨立 serial batch                      │           │
│  └───────────────┬──────────────┬──────────────────┘           │
│                  │              │                               │
│     ┌────────────┘              └───────────┐                  │
│     ▼                                       ▼                  │
│  ┌──────────────────────┐    ┌──────────────────────┐         │
│  │  Concurrent Batch     │    │  Serial Batch        │         │
│  │  (max 10 parallel)    │    │  (one at a time)     │         │
│  │                       │    │                      │         │
│  │  FileRead, Glob,     │    │  Bash, FileWrite,    │         │
│  │  Grep, WebFetch...   │    │  FileEdit, Agent...  │         │
│  └──────────┬───────────┘    └──────────┬───────────┘         │
│             │                           │                      │
│             └──────────┬────────────────┘                      │
│                        ▼                                       │
│  ┌─────────────────────────────────────────────────┐          │
│  │  Per-tool: canUseTool() — Permission Check       │          │
│  │  (詳見 Section 5: HITL)                          │          │
│  └─────────────────────┬───────────────────────────┘          │
│                        │                                       │
│                        ▼                                       │
│  ┌─────────────────────────────────────────────────┐          │
│  │  tool.call(input, context) — 實際執行            │          │
│  └─────────────────────┬───────────────────────────┘          │
│                        │                                       │
│                        ▼                                       │
│  ┌─────────────────────────────────────────────────┐          │
│  │  yield tool_result → 加入 messages               │          │
│  └─────────────────────────────────────────────────┘          │
└───────────────────────────────────────────────────────────────┘
```

**StreamingToolExecutor (最佳化路徑):**
當啟用時，工具不等模型完整回應結束就開始執行——`tool_use` block 串流完成後立即啟動，與模型後續輸出平行運作。結果依序緩衝，確保 API 消息順序正確。

---

## 4. 計畫模式 (Plan Mode)

Plan Mode 是一種特殊的權限狀態，讓模型只能使用唯讀工具來探索代碼庫，不能進行修改。

```
┌───────────────────────────────────────────────────────────────────┐
│                         Plan Mode Flow                              │
│                                                                    │
│  ┌──────────┐        ┌────────────────────────────────┐           │
│  │ 模型判斷  │───────►│ 呼叫 EnterPlanMode tool        │           │
│  │ 需要計畫  │        │ (自主決定或使用者要求)           │           │
│  └──────────┘        └──────────────┬─────────────────┘           │
│                                     │                              │
│                                     ▼                              │
│                      ┌──────────────────────────────┐             │
│                      │ permissionMode → 'plan'       │             │
│                      │                               │             │
│                      │ 可用工具限制為：               │             │
│                      │  ✓ FileRead                    │             │
│                      │  ✓ Glob / Grep                │             │
│                      │  ✓ WebFetch / WebSearch        │             │
│                      │  ✓ AskUserQuestion             │             │
│                      │  ✗ FileWrite / FileEdit        │             │
│                      │  ✗ Bash (寫入類)               │             │
│                      │  ✗ Agent (子代理)              │             │
│                      └──────────────┬─────────────────┘           │
│                                     │                              │
│                                     ▼                              │
│                      ┌──────────────────────────────┐             │
│                      │ 模型進行：                     │             │
│                      │  1. 探索 codebase             │             │
│                      │  2. 理解架構/模式              │             │
│                      │  3. 制定實施計畫               │             │
│                      │  4. 向使用者呈現計畫           │             │
│                      │  5. (可選) AskUserQuestion    │             │
│                      └──────────────┬─────────────────┘           │
│                                     │                              │
│                                     ▼                              │
│                      ┌──────────────────────────────┐             │
│                      │ 呼叫 ExitPlanMode tool        │             │
│                      │ permissionMode → 'default'    │             │
│                      │ 開始實際執行計畫               │             │
│                      └──────────────────────────────┘             │
└───────────────────────────────────────────────────────────────────┘
```

**觸發方式：**
- 模型自主判斷任務複雜，主動呼叫 `EnterPlanMode`
- 使用者要求 "plan this first" 或設定 `--plan` flag
- Coordinator Mode 下可指定 worker 以 plan mode 啟動

---

## 5. 人機互動 (HITL) — Permission System

每次工具被呼叫前，都必須通過權限系統。這是 Claude Code 防止意外破壞的核心機制。

```
┌───────────────────────────────────────────────────────────────────────┐
│                    Permission Decision Flow                             │
│                    (useCanUseTool / canUseTool)                         │
│                                                                        │
│  tool_use block + input                                                │
│         │                                                              │
│         ▼                                                              │
│  ┌─────────────────────────────────────────┐                          │
│  │ Step 1: hasPermissionsToUseTool()        │                          │
│  │                                          │                          │
│  │  檢查靜態規則 (settings.json):           │                          │
│  │   - alwaysAllow rules → 'allow'          │                          │
│  │   - alwaysDeny rules  → 'deny'           │                          │
│  │   - 其他              → 'ask'            │                          │
│  └────────────┬─────────────────────────────┘                          │
│               │                                                        │
│        ┌──────┼──────────────────┐                                     │
│        │      │                  │                                     │
│        ▼      ▼                  ▼                                     │
│   ┌────────┐ ┌────────┐  ┌────────────────────────┐                  │
│   │ allow  │ │ deny   │  │ ask                     │                  │
│   │        │ │        │  │                         │                  │
│   │ 直接   │ │ 回傳   │  │  進入互動判斷流程：     │                  │
│   │ 執行   │ │ 錯誤   │  │                         │                  │
│   └────────┘ └────────┘  └───────────┬────────────┘                  │
│                                      │                                 │
│                                      ▼                                 │
│                    ┌──────────────────────────────────┐                │
│                    │ Step 2: 自動分類器 (Classifier)   │                │
│                    │                                   │                │
│                    │ Bash 命令安全分類:                │                │
│                    │  - "高信心安全" → auto-allow      │                │
│                    │  - 不確定      → 繼續到 Step 3   │                │
│                    └──────────────────┬───────────────┘                │
│                                      │                                 │
│                                      ▼                                 │
│                    ┌──────────────────────────────────┐                │
│                    │ Step 3: 互動式權限提示            │                │
│                    │                                   │                │
│                    │ 向使用者顯示：                    │                │
│                    │  "Allow [tool] with [input]?"    │                │
│                    │                                   │                │
│                    │  使用者選擇：                     │                │
│                    │   [y] Allow once                  │                │
│                    │   [n] Deny                        │                │
│                    │   [a] Always allow this pattern   │                │
│                    └──────────────────────────────────┘                │
│                                                                        │
│  ┌────────────────────────────────────────────────────────────────┐   │
│  │ 特殊路徑:                                                       │   │
│  │  - Coordinator Mode: 由 coordinator 代為決定                    │   │
│  │  - Swarm Worker: 由 team leader 代為決定                        │   │
│  │  - Non-interactive (SDK/headless): 依 shouldAvoidPermission     │   │
│  │    Prompts flag 自動 deny                                       │   │
│  └────────────────────────────────────────────────────────────────┘   │
└───────────────────────────────────────────────────────────────────────┘
```

**權限模式 (PermissionMode):**
| Mode | 行為 |
|------|------|
| `default` | 標準模式，危險操作需要確認 |
| `plan` | 唯讀模式，只有讀取工具被允許 |
| `bypassPermissions` | 全部允許 (YOLO mode / `--dangerously-skip-permissions`) |

---

## 6. 停止判斷邏輯 (Stop / Terminal Decision)

當模型回應中**沒有** `tool_use` blocks (`needsFollowUp == false`) 時，系統進入停止判斷流程。

```
┌───────────────────────────────────────────────────────────────────────┐
│                     Stop Decision Flow                                  │
│                                                                        │
│  Model response has no tool_use blocks                                 │
│         │                                                              │
│         ▼                                                              │
│  ┌─────────────────────────────────────────────────┐                  │
│  │ Recovery Check 1: prompt-too-long (413)?         │                  │
│  │                                                  │                  │
│  │  YES → contextCollapse.recoverFromOverflow()    │                  │
│  │      → reactiveCompact (摘要壓縮)               │                  │
│  │      → continue (重試) 或 return 'prompt_too_long' │               │
│  └───────────────────────────┬─────────────────────┘                  │
│                              │ NO                                      │
│                              ▼                                         │
│  ┌─────────────────────────────────────────────────┐                  │
│  │ Recovery Check 2: max_output_tokens?             │                  │
│  │                                                  │                  │
│  │  YES, attempt < 3 →                             │                  │
│  │    注入 "Resume directly, break into smaller    │                  │
│  │    pieces" 訊息 → continue (重試)               │                  │
│  │                                                  │                  │
│  │  YES, attempt >= 3 → 放棄，surface error        │                  │
│  └───────────────────────────┬─────────────────────┘                  │
│                              │ NO                                      │
│                              ▼                                         │
│  ┌─────────────────────────────────────────────────┐                  │
│  │ API Error Message?                               │                  │
│  │                                                  │                  │
│  │  YES → return { reason: 'completed' }           │                  │
│  │        (不跑 stop hooks，避免死循環)             │                  │
│  └───────────────────────────┬─────────────────────┘                  │
│                              │ NO                                      │
│                              ▼                                         │
│  ┌─────────────────────────────────────────────────┐                  │
│  │ handleStopHooks() — 使用者定義的 Stop Hooks      │                  │
│  │                                                  │                  │
│  │  功能：                                          │                  │
│  │   - 執行 lint / test / format 檢查              │                  │
│  │   - 驗證程式碼品質                               │                  │
│  │   - 觸發 memory extraction                      │                  │
│  │   - 觸發 auto-dream (背景優化)                   │                  │
│  │                                                  │                  │
│  │  結果：                                          │                  │
│  │   - preventContinuation=true                    │                  │
│  │     → return 'stop_hook_prevented'              │                  │
│  │   - blockingErrors.length > 0                   │                  │
│  │     → 將錯誤注入 messages, continue (重試修復)   │                  │
│  │   - 正常通過                                     │                  │
│  │     → 繼續往下                                   │                  │
│  └───────────────────────────┬─────────────────────┘                  │
│                              │                                         │
│                              ▼                                         │
│  ┌─────────────────────────────────────────────────┐                  │
│  │ Token Budget Check (實驗性)                      │                  │
│  │                                                  │                  │
│  │  budget 未用完 → 注入 nudge message, continue    │                  │
│  │  budget 已滿   → 繼續往下                        │                  │
│  └───────────────────────────┬─────────────────────┘                  │
│                              │                                         │
│                              ▼                                         │
│  ┌─────────────────────────────────────────────────┐                  │
│  │ return { reason: 'completed' }                   │                  │
│  │                                                  │                  │
│  │ ✅ 任務完成，回到 QueryEngine                    │                  │
│  └─────────────────────────────────────────────────┘                  │
└───────────────────────────────────────────────────────────────────────┘
```

**所有 Terminal 原因一覽：**

| Reason | 觸發時機 |
|--------|---------|
| `completed` | 正常完成 (模型無更多 tool calls) |
| `aborted_streaming` | 使用者按 Ctrl+C (模型串流中) |
| `aborted_tools` | 使用者按 Ctrl+C (工具執行中) |
| `model_error` | API 呼叫失敗 |
| `image_error` | 圖片處理錯誤 |
| `blocking_limit` | Token 超過硬上限 |
| `prompt_too_long` | 413 錯誤且 recovery 失敗 |
| `stop_hook_prevented` | Stop hook 主動阻止繼續 |
| `hook_stopped` | PreToolUse hook 阻止工具執行 |
| `max_turns` | 達到設定的最大回合數 |

---

## 7. 子代理 (Sub-Agent / AgentTool)

Claude Code 透過 `AgentTool` 實現「分工」——主線程可以 spawn 子代理來平行處理子任務。

```
┌───────────────────────────────────────────────────────────────────────┐
│                      Sub-Agent Architecture                             │
│                                                                        │
│  ┌─────────────────────────────────────────────────────────────────┐  │
│  │                    Main Thread (Coordinator)                      │  │
│  │                                                                  │  │
│  │   模型決定需要 spawn 子代理：                                     │  │
│  │   tool_use: { name: "Agent", input: {                            │  │
│  │     prompt: "...",                                                │  │
│  │     description: "...",                                           │  │
│  │     run_in_background: true/false,                                │  │
│  │     subagent_type: "code" | "research" | custom_agent            │  │
│  │   }}                                                              │  │
│  └──────────────────────────────┬──────────────────────────────────┘  │
│                                 │                                      │
│              ┌──────────────────┼──────────────────┐                  │
│              │                  │                   │                  │
│              ▼                  ▼                   ▼                  │
│  ┌────────────────┐  ┌────────────────┐  ┌────────────────┐         │
│  │ Sync Agent      │  │ Async Agent    │  │ Coordinator    │         │
│  │ (前景)          │  │ (背景)         │  │ Mode Workers   │         │
│  │                 │  │                │  │                │         │
│  │ 主線程等待      │  │ 平行運行       │  │ 多個 workers   │         │
│  │ 結果回傳        │  │ 完成後通知     │  │ 各自獨立       │         │
│  └────────┬───────┘  └────────┬───────┘  └────────┬───────┘         │
│           │                   │                    │                  │
│           ▼                   ▼                    ▼                  │
│  ┌────────────────────────────────────────────────────────────┐      │
│  │              Each Sub-Agent runs its own:                    │      │
│  │                                                             │      │
│  │   ┌─────────────────────────────────────┐                  │      │
│  │   │ query() — 獨立的 Agent Loop          │                  │      │
│  │   │                                      │                  │      │
│  │   │  • 自己的 system prompt              │                  │      │
│  │   │  • 自己的 messages history           │                  │      │
│  │   │  • 受限的 tool set:                  │                  │      │
│  │   │    - Bash, FileRead, FileEdit,       │                  │      │
│  │   │      FileWrite, Glob, Grep,          │                  │      │
│  │   │      WebFetch, WebSearch, etc.       │                  │      │
│  │   │    - 不能 spawn 更多 Agent           │                  │      │
│  │   │    - 不能 EnterPlanMode              │                  │      │
│  │   │  • 獨立的 abort controller           │                  │      │
│  │   │  • 權限: shouldAvoidPermissionPrompts│                  │      │
│  │   └─────────────────────────────────────┘                  │      │
│  └────────────────────────────────────────────────────────────┘      │
│                                                                       │
│  ┌────────────────────────────────────────────────────────────┐      │
│  │  Coordinator Mode (多代理協作):                              │      │
│  │                                                             │      │
│  │  Coordinator 只有:                                          │      │
│  │    - AgentTool (spawn workers)                              │      │
│  │    - SendMessage (溝通)                                     │      │
│  │    - TaskStop                                               │      │
│  │                                                             │      │
│  │  Workers 有完整工具集 (不含 Agent)                           │      │
│  │  結果透過 task-notification XML 回報                         │      │
│  └────────────────────────────────────────────────────────────┘      │
└───────────────────────────────────────────────────────────────────────┘
```

**隔離模式 (Isolation):**
- `worktree`: 在 git worktree 副本中工作，避免衝突
- `remote`: 在遠端 CCR 環境執行 (僅限內部)
- 無隔離: 共享同一 working directory

---

## 8. 完整流程 — 一個請求的生命週期

以使用者輸入 "Add a login feature" 為例：

```
使用者: "Add a login feature"
    │
    ▼
QueryEngine.submitMessage("Add a login feature")
    │
    ├─ 組裝 system prompt + user context
    ├─ processUserInput (解析輸入)
    ├─ 加入 messages[]
    │
    ▼
query() 啟動
    │
    ╔══════════════════════════════════════════════════════════╗
    ║  Turn 1: 模型分析任務，決定先探索                        ║
    ║                                                          ║
    ║  Model response:                                         ║
    ║    "Let me explore the codebase first."                  ║
    ║    tool_use: [FileRead("src/app.ts"),                    ║
    ║              Glob("**/*.ts"),                             ║
    ║              Grep("auth|login")]                          ║
    ║                                                          ║
    ║  → needsFollowUp = true                                  ║
    ║  → 3 tools are concurrencySafe → 平行執行                ║
    ║  → Permission: all read-only → auto-allow                ║
    ║  → 結果注入 messages                                     ║
    ║  → continue → Turn 2                                     ║
    ╚══════════════════════════════════════════════════════════╝
    │
    ╔══════════════════════════════════════════════════════════╗
    ║  Turn 2: 模型制定計畫                                    ║
    ║                                                          ║
    ║  Model response:                                         ║
    ║    "Here's my plan: 1. Create auth module..."            ║
    ║    tool_use: [FileWrite("src/auth/login.ts", ...)]       ║
    ║                                                          ║
    ║  → needsFollowUp = true                                  ║
    ║  → FileWrite is NOT concurrencySafe → serial             ║
    ║  → Permission: FileWrite → 'ask'                         ║
    ║    → 向使用者顯示確認提示                                ║
    ║    → 使用者選 [y] Allow                                  ║
    ║  → 執行寫入                                              ║
    ║  → continue → Turn 3                                     ║
    ╚══════════════════════════════════════════════════════════╝
    │
    ╔══════════════════════════════════════════════════════════╗
    ║  Turn 3: 模型認為完成                                    ║
    ║                                                          ║
    ║  Model response:                                         ║
    ║    "I've added the login feature. Here's what I did..."  ║
    ║    (no tool_use blocks)                                   ║
    ║                                                          ║
    ║  → needsFollowUp = false                                 ║
    ║  → handleStopHooks():                                    ║
    ║    → 執行 lint hook → pass                               ║
    ║    → 執行 test hook → pass                               ║
    ║    → blockingErrors = []                                  ║
    ║    → preventContinuation = false                          ║
    ║  → return { reason: 'completed' }                        ║
    ╚══════════════════════════════════════════════════════════╝
    │
    ▼
QueryEngine: yield result message to SDK/UI
```

---

## 9. 關鍵設計原則

1. **模型自主判斷何時停止** — 沒有 tool_use blocks 即視為完成意圖，系統只在 stop hooks 要求時強制繼續
2. **權限是最後一道防線** — 即使模型決定執行危險操作，HITL 權限系統仍可阻擋
3. **Recovery 優先於失敗** — prompt-too-long 和 max_output_tokens 都有多層 recovery 機制
4. **工具平行化** — 唯讀工具平行執行、串流中即開始執行，最大化利用等待時間
5. **子代理隔離** — Sub-agent 有獨立的 context、受限的工具集、獨立的 abort 控制
6. **無限循環保護** — max_turns、token budget、reactive compact failure 都能打斷無限循環

---

## 10. 原始碼對照

| 概念 | 原始碼位置 |
|------|-----------|
| 主迴圈 | `src/query.ts` → `queryLoop()` |
| Session 生命週期 | `src/QueryEngine.ts` → `submitMessage()` |
| 工具分派 | `src/services/tools/toolOrchestration.ts` → `runTools()` |
| 串流工具執行 | `src/services/tools/StreamingToolExecutor.ts` |
| 權限系統 | `src/hooks/useCanUseTool.tsx` |
| 停止邏輯 | `src/query/stopHooks.ts` → `handleStopHooks()` |
| Sub-Agent | `src/tools/AgentTool/AgentTool.tsx` |
| Plan Mode | `src/tools/EnterPlanModeTool/EnterPlanModeTool.ts` |
| Coordinator Mode | `src/coordinator/coordinatorMode.ts` |
| Tool 介面定義 | `src/Tool.ts` |
| 工具註冊 | `src/tools.ts` → `getAllBaseTools()` |
| 自動壓縮 | `src/services/compact/autoCompact.ts` |

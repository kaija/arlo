# Arlo-Rust Agent Framework 系統架構

> English version: [agent-framework.md](agent-framework.md)

本文件描述 arlo-rust 的核心執行架構：主迴圈（RunLoop）的自主決策、HITL 權限控制、Task／Todo 管理、Sub-Agent 的運作與協調，以及錯誤處理流程。最後以一個貫穿所有機制的完整範例收尾。

程式碼位置以 `crates/agent-core/src/` 為基準，例如 `run_loop.rs` 指 `crates/agent-core/src/run_loop.rs`。

---

## 目錄

1. [整體架構總覽](#1-整體架構總覽)
2. [主迴圈（RunLoop）與自主決策](#2-主迴圈runloop與自主決策)
3. [HITL 權限控制](#3-hitl-權限控制human-in-the-loop)
4. [Task 管理與 Todo 工具](#4-task-管理與-todo-工具)
5. [Sub-Agent 運作機制](#5-sub-agent-運作機制)
6. [Main Agent 對多個 Sub-Agent 的協調](#6-main-agent-對多個-sub-agent-的協調)
7. [錯誤處理與復原流程](#7-錯誤處理與復原流程)
8. [貫穿範例：一次長任務的完整生命週期](#8-貫穿範例一次長任務的完整生命週期)

---

## 1. 整體架構總覽

Workspace 分為五個 crate：

| Crate | 職責 |
|---|---|
| `agent-core` | RunLoop、NextStep 狀態機、權限引擎、TaskStore、Sub-Agent、壓縮、復原 |
| `agent-llm` | Model / ModelProvider 實作（Anthropic、OpenAI 等） |
| `agent-tools` | 內建工具（file_read / file_edit / bash …） |
| `agent-mcp` | MCP client 與 transport |
| `agent-cli` | TUI、approval UI、事件渲染 |

核心資料流：

```
User prompt
    │
    ▼
run() / run_stream()  ──────────► RunEvent stream（TUI 訂閱）
    │
    ▼
┌─────────────────── drive()（主迴圈）───────────────────┐
│ Phase 0   turn limit 檢查                               │
│ Phase 0.5 背景任務結果注入（TaskStore 通知）             │
│ Phase 1   Context 壓縮（3 層 CompactionPipeline）        │
│ Phase 1.5 Input guardrails（僅第一輪）                  │
│ Phase 2   組裝 ModelRequest（system + messages + tools）│
│ Phase 3   串流模型回應（錯誤 → RecoveryTracker）         │
│ Phase 4   StreamingToolExecutor 併發執行工具             │
│ Phase 5   resolve_next_step() → NextStep                │
│ Phase 6   套用狀態轉移（continue / 終止 / 中斷 / 復原）  │
└─────────────────────────────────────────────────────────┘
    │                         │
    ▼                         ▼
PermissionEngine          TaskStore（TaskEntry + TodoItem）
（HITL 決策）              （背景任務 + 計畫清單）
                              ▲
                              │
                        SubAgentTool（fg/bg 生成子代理）
```

`run()`（非串流）與 `run_stream()`（串流）共用同一個 `drive()` 實作（`run_loop.rs`）。串流模式下每個 phase 都會透過 mpsc channel 發出 `RunEvent`（`TurnStart`、`StreamChunk`、`ToolStart/ToolEnd`、`StepResolved`、以及唯一一個終止事件）。

---

## 2. 主迴圈（RunLoop）與自主決策

### 2.1 NextStep 狀態機

每一輪（turn）結束時，`resolve_next_step()`（`run_loop.rs`）根據模型的 stop reason、工具呼叫與權限決策，解析出一個 `NextStep`（`next_step.rs`）：

```rust
pub enum NextStep {
    Continue,                                  // 有工具呼叫且全部允許 → 繼續下一輪
    FinalOutput { text, structured },          // 模型結束回合 → 候選終止
    Interruption { pending: Vec<PendingApproval> }, // 工具需要用戶核准
    Recovery { strategy: RecoveryStrategy },   // 可復原的錯誤
    MaxTurns { count },                        // 到達回合上限
    Aborted { reason },                        // 中止（content filter、拒絕、超預算）
}
```

判斷順序（`resolve_next_step`）：

1. `StopReason::ContentFilter` → `Aborted`
2. `current_turn + 1 >= max_turns` → `MaxTurns`
3. `StopReason::ToolUse` 且有工具呼叫 → 逐一過 `PermissionEngine.check()`：
   - 任一 `Deny` → `Aborted`（安全優先，直接中止）
   - 任一 `NeedsApproval` → `Interruption`（收集所有待核准項目）
   - 全部 `Allow` → `Continue`
4. `StopReason::MaxTokens` → `Recovery`（先 ContinueMessage，兩次後升級 EscalateOutputTokens）
5. `EndTurn` / `StopSequence` → `FinalOutput`

### 2.2 自主判斷「是否要繼續」

關鍵設計：**模型說「我講完了」（`FinalOutput`）不代表迴圈真的結束**。在真正回覆用戶之前，`drive()` 依序做三道「續跑檢查」：

**檢查 1 — Output guardrails**：先驗證最終輸出；未通過直接以 `RunError::Guardrail` 終止。

**檢查 2 — 背景任務未完成 → 不准結束**（`await_background_tasks`）：
如果 TaskStore 裡還有 `Pending` / `Running` 的背景任務（通常是 background sub-agent），迴圈會阻塞等待（200ms 輪詢，上限 10 分鐘），直到至少一個任務到達終止狀態，把結果包成 `[background task completed/failed]` 的 user message 注入對話，然後 `continue` 回主迴圈讓模型針對結果反應。這保證 main agent 永遠不會在 sub-agent 還在跑時就對用戶說「做完了」。

**檢查 3 — Todo 未完成 → 注入續跑提示**（`todo_continuation_prompt`）：
如果 TodoList 還有非 `Completed` 的項目，注入一則列出未完成項目的 user message（`You have N incomplete todo item(s). Continue working through them: …`），讓模型繼續執行。為避免模型卡死造成無限迴圈，**連續 todo 續跑最多 3 次**（`todo_continuation_count`，任何一次正常 `Continue` 都會歸零重計）。

三道檢查都通過後，才發出 `AgentEnd` 事件並回傳 `RunResult` 給用戶。

### 2.3 何時回應用戶、等待用戶輸入

迴圈把控制權交還用戶的時機只有幾種：

| 情境 | 行為 |
|---|---|
| `FinalOutput` 且無未完成任務/todo | 回傳最終答案，run 結束 |
| `Interruption` 且**有** `approval_handler` | 不返回——inline 等待 handler（TUI 彈出核准 UI），拿到決定後繼續迴圈 |
| `Interruption` 且**無** handler | 把 `pending_approvals` 記進 `RunState` 後返回 `RunResult`；呼叫端之後可用 `Input::Resume { state }` 續跑 |
| `MaxTurns` / `Aborted` / Recovery 耗盡 | 帶著目前狀態返回或回傳錯誤 |

也就是說「等待用戶提供更多資訊」有兩種形態：**同步 HITL**（approval handler 阻塞在 UI 上）與**非同步暫停**（無 handler 時把狀態序列化返回，之後 Resume）。

### 2.4 每輪的資源護欄

- **Turn limit**：`agent.max_turns` 覆寫 `config.max_turns`，Phase 0 檢查。
- **Budget**：每輪累計 usage 換算成本（`accumulate_usage`），超過 `config.budget_usd` 立即 `Aborted("budget_exceeded")`。
- **Context 壓縮**：每輪 Phase 1 跑 3 層 `CompactionPipeline`（`compaction/mod.rs`）——由輕到重：`tools_compact`（清除過期工具結果，零成本）→ `session_memory`（注入 session 記憶，零成本）→ `full_summarize`（一次 LLM 呼叫做結構化摘要）。任一層把 token 壓到門檻以下即停；連續失敗 3 次觸發斷路器停用壓縮。
- **串流消費者斷線**：每輪開始時發 `TurnStart`，發送失敗（stream 被 drop）即以 `Aborted("stream_dropped")` 結束，避免孤兒 run 繼續燒錢。

---

## 3. HITL 權限控制（Human-in-the-Loop）

### 3.1 兩個層次：Tool 宣告 + 引擎裁決

每個工具透過 `Tool::approval_requirement()`（`tool.rs`）宣告自身風險等級：

```rust
pub enum ApprovalRequirement {
    Never,                 // 從不需核准（預設，如唯讀工具）
    Always,                // 每次都要核准（如 bash、file_write）
    Conditional(String),   // 條件式，附說明（如「寫入 /etc 時」）
}
```

實際裁決由 `PermissionEngine`（`permission.rs`）做，**4 層短路評估**：

```
Layer 1  Mode        Bypass → 全放行；DenyAll → 全拒絕；Normal → 往下
Layer 2  靜態規則     static_deny 命中 → Deny（deny 優先於 allow）
                     static_allow 命中 → Allow
Layer 3  Session 規則 本地 session_allows 或共享 shared_session_grants 命中 → Allow
Layer 4  工具宣告     Never → Allow；Always/Conditional → NeedsApproval
```

規則支援 pattern（`pattern.rs` 的 `ToolPattern`）：裸名稱（`bash`）、glob（`fs_*`）、複合式（`Bash(npm*)` —— 同時比對工具名與參數內容）。靜態規則可由設定檔載入（`settings.rs` 的 `MergedPolicy`）。

### 3.2 核准流程（Interruption）

當 `resolve_next_step` 收集到 `NeedsApproval` 的工具呼叫，回傳 `NextStep::Interruption { pending }`，主迴圈交給 `ApprovalHandler`（`config.rs`）：

```rust
pub trait ApprovalHandler: Send + Sync {
    async fn request_approval(&self, context: &ApprovalContext) -> Vec<ApprovalResponse>;
}

pub enum ApprovalResponse {
    Allow,                            // 只放行這一次
    Deny,                             // 拒絕這一次
    AlwaysAllow { pattern: String },  // 放行並註冊 session 級 pattern grant
}
```

`ApprovalContext.agent_name` 標明是哪個（sub-）agent 在要求核准，TUI 可以據此顯示來源。回應處理（`drive()` 的 `Interruption` 分支）：

- **Allow**：保留該工具結果，正常寫入對話。
- **AlwaysAllow**：呼叫 `permissions.grant_session_allow(pattern)`——之後同 pattern 的呼叫在 Layer 3 直接放行，不再打擾用戶；同時保留本次結果。
- **Deny**：丟棄工具結果，改注入一則 `is_error: true` 的 ToolResult（`Permission denied: tool 'x' was not approved by the user.`），**run 不中止**——模型會在下一輪看到拒絕訊息並自行調整做法。

注意與 Layer 2 `Deny` 的差別：**靜態 deny 直接 `Aborted` 整個 run**（policy 層的硬規則）；**用戶互動式 Deny 只是拒絕單次呼叫**，對話繼續。

無 handler 時（CI、非互動模式）有兩種選擇：不設 handler → run 暫停返回（見 2.3）；或掛 `DenyAllApprovalHandler` → 全部自動拒絕並記 warn log，迴圈不阻塞。

### 3.3 Session grant 的跨 agent 共享

`PermissionEngine` 可掛一個 `Arc<RwLock<Vec<ToolPattern>>>` 共享 grant 儲存（`with_shared_session_grants`）。Sub-agent 生成時（`sub_agent.rs::sub_agent_config`）會接上共享儲存，因此**用戶在委派過程中按下的 AlwaysAllow，整棵 agent 樹都看得到**，不會每個 sub-agent 都重複問一次。安全底線不變：session grant 位於 Layer 3，永遠壓不過 Layer 2 的 static_deny。

---

## 4. Task 管理與 Todo 工具

### 4.1 兩種實體：TaskEntry vs TodoItem

`task_store.rs` 定義了兩個**用途不同但共存於同一個 `TaskStore`** 的實體：

| | `TodoItem`（計畫層） | `TaskEntry`（執行層） |
|---|---|---|
| 代表什麼 | 模型的**工作計畫項目**（給用戶看的 checklist） | 一個**背景執行單位**（通常是 background sub-agent） |
| 誰建立 | 模型透過 `todolist` 工具主動維護 | `SubAgentTool` 在 spawn 背景任務時自動註冊 |
| 狀態 | `Pending → InProgress → Completed` | `Pending → Running → Completed / Failed / Killed` |
| 對主迴圈的影響 | 未完成 → 觸發 todo 續跑提示（最多連續 3 次） | 未終止 → `FinalOutput` 被攔下，迴圈等待結果 |
| 額外欄位 | `active_form`（顯示用） | `output`、`usage`、`dependencies`、`max_retries`、`acknowledged` |

兩者的關聯是**間接的、由模型語意銜接**：框架不強制 todo 與 task 一對一對應。典型模式是模型先用 todolist 拆解計畫（「1. 分析 A 模組 2. 分析 B 模組 3. 彙整」），再對其中可平行的項目各 spawn 一個 background sub-agent（產生 TaskEntry），收到結果通知後回頭把對應 todo 勾成 completed。主迴圈的兩道續跑檢查（背景任務 + todo）共同保證這個閉環在計畫全部完成前不會斷掉。

### 4.2 TodoListTool（模型面向的計畫工具）

`todolist_tool.rs` 實作 `Tool` trait，動作：`add`（content ≤1000 字、active_form ≤200 字）、`update`（改狀態）、`list`、`remove`、`clear_completed`。`Concurrency::Safe`、`ApprovalRequirement::Never`（預設）——維護計畫不需要用戶核准。

### 4.3 TaskStore trait 與生命週期

`TaskStore`（async trait，記憶體實作在 `in_memory_task_store.rs`）提供：

- **CRUD 與狀態機**：`create_task`（進 `Pending`）、`transition_task`（驗證合法轉移；進終止態時記 `completed_at`；Failed 且還有 retry 額度時重置回 `Pending` 並 `retry_count += 1`）。
- **查詢**：`list_tasks`、`count_by_status`、`list_ready_tasks`（Pending 且依賴全滿足）、`list_blocked_tasks`（依賴未滿足）。
- **通知協定**：`list_unacknowledged_terminal` + `acknowledge_task` —— 這是結果送回模型「恰好一次」的關鍵（見第 6 節）。
- **GC**：`evict_acknowledged`、`evict_older_than`。

`TaskEntry.dependencies` 支援任務間依賴（B 等 A 完成才 ready）；依賴失敗以 `TaskStoreError::DependencyFailed` 呈現。

### 4.4 Agent 如何用 Task 管理長任務

長任務的標準模式：

1. **拆解**：模型用 `todolist add` 建立可見計畫，逐項 `update` 為 `in_progress` → `completed`。
2. **委派**：耗時且可獨立的子工作交給 background sub-agent，主對話不被長時間工具呼叫卡住，模型可以繼續處理其他 todo。
3. **防早退**：`FinalOutput` 被背景任務檢查與 todo 檢查雙重攔截，模型「忘記還有事」時框架會把它拉回來。
4. **可恢復**：run 因 Interruption（無 handler）暫停時，`RunState`（含 messages、pending_approvals）可序列化，之後 `Input::Resume { state }` 續跑。

---

## 5. Sub-Agent 運作機制

### 5.1 定義與註冊

Sub-agent 以 `SubAgentDef`（`agent.rs`）掛在父 agent 上，實質是一個完整的 `Agent`（有自己的 instructions、tools、max_turns）包裝成父 agent 的一個工具（`SubAgentTool`，`sub_agent.rs`）：

```rust
pub struct SubAgentDef {
    pub agent: Arc<Agent>,          // 完整的子代理定義
    pub tool_name: Option<String>,  // 曝露給模型的工具名
    pub max_turns: Option<u32>,     // 覆寫父 config 的回合上限
    pub background: bool,           // 前景 or 背景模式
}
```

模型呼叫這個工具時傳 `{"task": "..."}`，`task` 字串成為 sub-agent 的初始 prompt。

### 5.2 隔離模型（Claude-Code isolation）

Sub-agent 在**全新的 RunLoop、空白訊息歷史**中啟動——看不到父對話，防止 context 污染，也讓父對話不必背負子任務的大量中間過程（只拿到最終結果）。設定繼承規則（`sub_agent_config()`）：

- 複製父 `RunConfig`，`max_turns` 可被 `def.max_turns` 覆寫。
- `agent_name` 設為子代理名稱 → 核准請求會標明來源。
- `approval_handler` 透過 `Arc` 共享 → **子代理的 HITL 核准直接彈到父層的 UI**。
- 掛上共享 session grant store → AlwaysAllow 跨 agent 生效（見 3.3）。

### 5.3 前景模式（`background: false`）

`run_foreground()`：同步 `run()` 子代理到完成，最終輸出以 `ToolOutput::Text` 回給父模型。子代理撞到 max_turns 時在輸出附註 `[Sub-agent reached turn limit of N]`；子代理出錯回 `ToolOutput::Error`（**對父 run 非致命**——父模型看到錯誤字串後自行決定重試或改道）。

### 5.4 背景模式（`background: true`）

`run_background()`，有 TaskStore 時的完整流程：

1. **先註冊、後 spawn**：先 `create_task`（描述含 agent 名與 prompt 前 80 字）拿到 `task_id`，確保父模型在收到工具回覆時就有可關聯的 ID。
2. Spawn detached tokio task：轉移 `Running` → 執行 `run()` → 成功則 `transition_task(Completed, output)` + `update_task_usage`；失敗則 `transition_task(Failed, error)`。
3. **Panic 隔離**：實際的 `run()` 再包一層 `tokio::spawn`，子代理 panic 時外層 bookkeeping task 仍能把狀態記成 Failed——否則 store 會永遠卡在 Running，主迴圈的等待邏輯就死鎖了。
4. 立即回覆父模型：`Background task started: task_id=…` 並明確提示「結果會以 [background task completed] 通知送達，**收到前不要下結論**」。

沒有 TaskStore 時退化為 fire-and-forget（結果丟失，只回一個遞增序號），僅適合不在乎結果的旁路任務。

---

## 6. Main Agent 對多個 Sub-Agent 的協調

### 6.1 併發執行

`SubAgentTool::concurrency()` 回傳 `Concurrency::Safe`——sub-agent 彼此隔離、不共享可變狀態，因此模型在**同一輪**發出的多個 sub-agent 呼叫會由 `StreamingToolExecutor`（`executor.rs`）平行執行（預設併發上限 8，`Exclusive` 工具則獨占執行）。搭配 background 模式，父模型可以一次撒出 N 個子任務後立刻繼續做別的事。

### 6.2 結果如何回到模型的對話（通知協定）

背景任務的結果**必須進入模型讀得到的對話**，而不是只到 UI。兩條路徑，共用同一個去重機制：

**路徑 A — 回合邊界注入**（`drain_task_notifications`，Phase 0.5）：
每輪開始時查 `list_unacknowledged_terminal()`，把每個已終止但未確認的任務包成：

```
[background task completed] Sub-agent 'researcher': …（task_id=…）
Result: <output>
```

（失敗則 `[background task failed]` + error）作為 user message 注入，並立刻 `acknowledge_task` ——**acknowledged 旗標保證每個結果恰好送達模型一次**，不會重複也不會遺漏。

**路徑 B — 終止前等待**（`await_background_tasks`，見 2.2 檢查 2）：
模型想收尾但還有任務在跑時，阻塞等到下一個任務終止，注入通知後續跑。

### 6.3 兩邊任務協調合作的保證

| 保證 | 機制 |
|---|---|
| 結果恰好一次送達模型 | `acknowledged` 旗標 + 注入後立即 acknowledge |
| 不會在子任務完成前對用戶收尾 | `FinalOutput` 前的 `await_background_tasks` 攔截 |
| 父模型能關聯「哪個委派 → 哪個結果」 | spawn 前先建 task 拿 `task_id`，通知中帶回同一 ID |
| 子代理 panic 不會造成永久等待 | 雙層 spawn 的 panic 隔離 → 記為 Failed → 照常通知 |
| 等待不會無限期 | 10 分鐘 deadline + 每次輪詢檢查 stream 是否已被 drop |
| HITL 一致性 | 共享 approval handler + 共享 session grants（agent 樹一體適用） |
| 成本可歸因 | 子代理 usage/cost 記入 TaskEntry.usage，父層可彙總 |

CLI 層（`agent-cli`）同樣讀 TaskStore 來渲染背景任務狀態，但那是 UI 顯示；模型的認知一律走上述對話注入路徑。更深入的時序圖與已知限制見 [sub-agent-task-coordination.md](sub-agent-task-coordination.md)。

---

## 7. 錯誤處理與復原流程

### 7.1 錯誤階層（`error.rs`）

```
RunError（run 層）
├── Model(ModelError)      ← API 錯誤 / RateLimited / PromptTooLong /
│                             MaxOutputTokens / Connection / StreamInterrupted
├── Tool(ToolError)        ← InvalidInput / ExecutionFailed / Timeout / NotAvailable
├── MaxTurns / BudgetExceeded / Guardrail
├── Aborted                ← content filter、權限 deny、stream 斷線、超預算
└── RecoveryExhausted      ← 復原重試耗盡
```

### 7.2 分層處理原則

**工具錯誤：非致命，回饋給模型。** 工具執行失敗不會終止 run——錯誤字串以 `is_error: true` 的 ToolResult 寫回對話，模型下一輪看到後自行修正（重試、換參數、換方法）。模型呼叫不存在的工具也一樣（`NotFoundTool` 佔位工具回報 `NotAvailable`）。前景 sub-agent 的失敗同理，以 `ToolOutput::Error` 呈現。

**模型錯誤：進復原系統。** `RecoveryTracker`（`recovery.rs`）把 `ModelError` 映射到策略並按錯誤變體分別計數：

| 錯誤 | 策略 |
|---|---|
| `PromptTooLong` | `CompactAndRetry` — 暴力裁掉最舊的非 system 訊息至半個 context window（正常壓縮是 Phase 1 pipeline 的事；這是 provider 仍拒絕時的最後手段） |
| `MaxOutputTokens` | 前 2 次 `ContinueMessage`（注入「請從中斷處繼續」）；第 3 次 `EscalateOutputTokens`（加倍 max_output_tokens，上限為模型硬上限） |
| `StreamInterrupted` | `ContinueMessage` |
| 其他（Api / RateLimited / Connection） | `GiveUp` |

每個錯誤變體**獨立計數**，超過 `MAX_RECOVERY_ATTEMPTS = 3` 即 `GiveUp` → run 以 `RunError::RecoveryExhausted` 結束。復原重試**不消耗 turn 數**；任何一次成功的 `Continue` 會 `reset()` 全部計數。

**Guardrail：硬性終止。** Input guardrails 在第一輪檢查輸入、output guardrails 在 `FinalOutput` 前檢查輸出，按註冊順序短路評估，任一失敗即發 `GuardrailTripped` 事件並回 `RunError::Guardrail`。

**背景任務錯誤：狀態化 + 通知。** Sub-agent 失敗/panic 記為 `TaskEntry::Failed`（含 `last_error`），以 `[background task failed]` 通知模型，由模型決定補救。`max_retries > 0` 時 store 會自動重置回 Pending 重試。

### 7.3 終止事件的唯一性

串流模式保證恰好一個終止事件：`AgentEnd`、`MaxTurns`、`Aborted`、`Error`、`Interruption`、`GuardrailTripped` 之一，TUI 據此收斂 UI 狀態。

---

## 8. 貫穿範例：一次長任務的完整生命週期

**情境**：用戶要求——「分析這個 repo 的 A、B 兩個模組的效能瓶頸，然後產出一份綜合報告寫入 report.md」。Main agent 掛了 `todolist`、`file_write`（`ApprovalRequirement::Always`）、以及一個 `background: true` 的 `analyzer` sub-agent。

**Turn 1 — 拆解計畫（Todo 工具）**
模型呼叫 `todolist add` ×3：「分析模組 A」「分析模組 B」「彙整報告寫入 report.md」。三個工具呼叫都是 `Never` 核准等級，PermissionEngine Layer 4 放行 → `NextStep::Continue`。

**Turn 2 — 平行委派（Sub-Agent + Task）**
模型同輪發出兩個 `analyzer` 呼叫（task 分別為模組 A、B），並把前兩個 todo 標為 `in_progress`。`SubAgentTool` 為 `Concurrency::Safe`，兩個呼叫由 executor 平行處理：各自先 `create_task` 拿到 `task_id=T1`、`T2`（`TaskEntry::Pending`），spawn detached task 後立刻回覆「Background task started: task_id=T1…收到通知前不要下結論」。兩個 sub-agent 在各自空白的 RunLoop 中開跑（`Running`），看不到父對話。

**Turn 3 — 子代理觸發 HITL**
sub-agent A 需要跑 `bash` 做 profiling（`Always`）。它自己的迴圈解析出 `Interruption`，透過**共享的 approval handler** 彈到父層 TUI，`ApprovalContext.agent_name = "analyzer"` 標明來源。用戶選 **AlwaysAllow(`Bash(cargo*)`)** → 寫入**共享 session grant store**。稍後 sub-agent B 跑到同樣的命令時，Layer 3 直接放行，用戶不會被問第二次。

**Turn 4 — 模型想提早收尾，被攔下**
模型此時無事可做，輸出「兩個分析已啟動，完成後我會彙整」並 `EndTurn` → `FinalOutput`。但 `await_background_tasks` 發現 T1、T2 仍在 `Running`，**攔下終止**並阻塞等待。期間 sub-agent A 內部撞到 `MaxOutputTokens`——它自己的 `RecoveryTracker` 注入 ContinueMessage 續寫，不影響父層。

**Turn 5 — 結果回流**
Sub-agent A 完成：`transition_task(T1, Completed, output)` + usage 入帳。等待中的父迴圈 drain 出 `[background task completed] … (task_id=T1) Result: <A 模組分析>`，acknowledge T1（恰好一次），注入 user message 後續跑。模型把 todo「分析模組 A」勾成 `completed`。B 完成時同理（若 B panic，則收到 `[background task failed]`，模型可改為 foreground 重試）。

**Turn 6 — 寫檔觸發 HITL（主層）**
模型彙整兩份結果，呼叫 `file_write` 寫 report.md。`Always` → `Interruption`，TUI 顯示 diff，用戶按 **Allow**（單次）。結果保留，todo 第三項勾成 `completed`。

**Turn 7 — 合法收尾**
模型輸出總結並 `EndTurn` → `FinalOutput`。三道檢查依序通過：output guardrails OK；TaskStore 無 Pending/Running（T1、T2 皆已 acknowledged）；TodoList 全部 `Completed`（若模型忘了勾第三項，這裡會注入續跑提示把它拉回來，最多 3 次）。發出 `AgentEnd`，`RunResult` 帶回輸出、累計 usage/cost 與完整 `RunState`。

這個範例走過了：NextStep 狀態機（Continue / Interruption / FinalOutput）、三道終止前檢查、4 層權限引擎與跨 agent 的 session grant、Todo 計畫層與 Task 執行層的分工、背景 sub-agent 的註冊-執行-通知-確認閉環、平行協調，以及模型層與任務層各自獨立的錯誤復原。

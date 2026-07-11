# Arlo

**Arlo** 是一個開源、以 Rust 打造的 agentic AI 框架 —— 可以把它想成是仿照 Claude
Code 提供的能力、但更精簡、更通用的版本。它提供一套具備串流輸出的 agent 執行迴圈，
內建工具呼叫、人機協作（HITL）權限控管、sub-agent、背景任務、MCP 整合與 Agent
Skills，並透過多種介面（互動式 TUI、單次 CLI 指令、可嵌入的函式庫）對外提供。

相較於綁定單一廠商的 Claude Code 產品，Arlo 是一個與 LLM 供應商無關的小型核心，
可以接上 Anthropic、OpenAI 相容 API，或是你自己架設的本地模型伺服器，並嵌入到任何
你需要的應用場景中。

> 🇺🇸 [English README](README.md) ・ 🇯🇵 [日本語版 README](README.ja.md)

## 為什麼選 Arlo

- **精簡而非陽春** —— 小巧的 Rust 核心（5 個 crate），而非單體式應用程式；工具、
  provider、權限規則等每一塊都可以替換。
- **不綁定 LLM 供應商** —— 內建 Anthropic 與 OpenAI 相容的 HTTP provider，並支援
  自訂 `base_url`，可接本地或自架模型伺服器。
- **多重介面** —— 互動式 TUI、單次 CLI 指令，以及可直接嵌入自家應用程式的 Rust
  函式庫。

## 功能一覽

| 功能 | 說明 |
|---|---|
| **Agentic 執行迴圈** | 以回合為單位的迴圈（壓縮上下文 → 發送請求 → 串流回應 → 執行工具 → 判斷下一步），具備型別化的狀態轉換與自動錯誤復原 |
| **MCP client** | 透過 Model Context Protocol（MCP）連接外部工具伺服器，與內建工具並用 |
| **Agent Skills** | 從專案層級（`.arlo/skills/`）與使用者層級（`~/.arlo/skills/`）目錄探索並載入以 Markdown 定義的技能，支援模板變數替換 |
| **自主 Agent 迴圈** | Agent 會在回合之間自行規劃、呼叫工具並反覆迭代，並串流輸出進度事件（`TurnStart`、`ToolStart`/`ToolEnd`、`StepResolved` 等） |
| **人機協作（HITL）權限** | 4 層權限引擎 —— glob 樣式規則（如 `shell(npm *)`、`file_write(/tmp/*)`）、靜態允許/拒絕清單，以及互動式核准提示 |
| **任務管理與 sub-agent** | 內建 Task/Todo 儲存機制；可派生前景或背景的 sub-agent，各自擁有獨立歷史紀錄 —— 背景結果會確保只回傳給父 agent 一次 |
| **內建工具** | 檔案讀取/寫入/編輯、glob、grep、shell、web fetch、web search |
| **上下文壓縮** | 三層壓縮管線 —— 工具結果壓縮 → 對話記憶 → 完整摘要化 —— 確保不超出上下文視窗 |
| **多重介面** | 互動式 TUI、單次 CLI 指令，以及可嵌入的 Rust crate |

## 專案結構

| Crate | 用途 |
|---|---|
| `agent-core` | 執行迴圈、權限、工具 trait、sub-agent、任務儲存、skills、上下文壓縮 |
| `agent-llm` | LLM providers（OpenAI 相容、Anthropic）、重試邏輯、模型覆寫 |
| `agent-tools` | 內建工具：檔案操作、glob、grep、shell、web fetch/search |
| `agent-mcp` | MCP client |
| `agent-cli` | `arlo` 執行檔：TUI、agent 組裝 |

## 快速開始

需要 stable 版 Rust（透過 [rustup](https://rustup.rs) 安裝）。

```bash
git clone <repo-url>
cd arlo-rust
cargo build --release
```

設定好你的 provider 憑證後執行：

```bash
export OPENAI_API_KEY="sk-..."
# 選用：指向任何 OpenAI 相容伺服器
# export OPENAI_BASE_URL="http://localhost:8000/v1"

cargo run -p agent-cli                          # 互動式 TUI
cargo run -p agent-cli -- "summarize this repo" # 單次指令
```

```
arlo [--model PROVIDER:MODEL] [--profile NAME] [--dump-prompt] ["prompt"]
```

不帶 prompt 參數會啟動互動式 REPL。

## 設定

執行期設定放在 `.arlo/settings.json`（專案層級）與 `~/.arlo/settings.json`
（使用者層級）；專案設定優先。

- **`permissions`** —— `allow`/`deny` 工具規則，例如 `"shell(cargo *)"`、
  `"web_fetch(https://docs.rs/*)"`
- **`profiles`** —— 具名的 LLM provider 設定（model、base URL、context window、
  最大輸出 token 數），可用 `--profile` 選用

Skills 放在 `.arlo/skills/`（專案層級）與 `~/.arlo/skills/`（使用者層級）。

完整參考請見 [doc/configuration.md](doc/configuration.md)。

## 文件

- [doc/configuration.md](doc/configuration.md) —— 設定、權限規則、profiles
- [doc/agent-framework.md](doc/agent-framework.md) —— 完整架構深入介紹
- [doc/sub-agent-task-coordination.md](doc/sub-agent-task-coordination.md) —— sub-agent
  與背景任務設計、時序圖、已知限制
- [AGENTS.md](AGENTS.md) —— 程式碼庫地圖與不變量（coding agent 也會讀取此檔）

## 開發

```bash
make check   # cargo check --workspace（快速）
make test    # cargo test --workspace
make lint    # clippy，警告視為錯誤
make fmt     # rustfmt
```

貢獻方式請見 [CONTRIBUTING.md](CONTRIBUTING.md)。

## 授權

採用 [MIT License](LICENSE) 授權。

# Arlo

**Arlo** は Rust 製のオープンソース agentic AI フレームワークです。Claude Code が
提供するような機能を模しつつ、よりシンプルで汎用的な AI エージェントを目指して
作られています。ツール呼び出し、Human-in-the-Loop（HITL）権限制御、sub-agent、
バックグラウンドタスク、MCP 連携、Agent Skills を備えたストリーミング実行ループを
提供し、対話型 TUI・ワンショット CLI・組み込み可能なライブラリなど複数のインター
フェースから利用できます。

単一プロバイダに紐づく製品である Claude Code とは異なり、Arlo は LLM プロバイダに
依存しない小さなコアです。Anthropic、OpenAI 互換 API、あるいは自前のローカルモデル
サーバーに接続し、必要な場所に自由に組み込むことができます。

> 🇺🇸 [English README](README.md) ・ 🇹🇼 [繁體中文版 README](README.zh-TW.md)

## Arlo を選ぶ理由

- **シンプルだが機能不足ではない** —— 巨大なモノリシックアプリではなく、5 つの
  crate からなる小さな Rust コア。ツール・プロバイダ・権限ルールなど、あらゆる
  パーツを差し替え可能。
- **プロバイダに依存しない** —— Anthropic と OpenAI 互換の HTTP プロバイダを
  標準サポート。`base_url` のカスタム指定でローカル／自前ホストのモデルにも接続可能。
- **複数のインターフェース** —— 対話型 TUI、ワンショット CLI、そして自作アプリに
  直接組み込める Rust ライブラリ。

## 機能一覧

| 機能 | 説明 |
|---|---|
| **Agentic 実行ループ** | ターン単位のループ（コンテキスト圧縮 → リクエスト送信 → ストリーミング応答 → ツール実行 → 次ステップの決定）。型付きの状態遷移と自動エラーリカバリを備える |
| **MCP クライアント** | Model Context Protocol（MCP）経由で外部ツールサーバーに接続し、組み込みツールと併用可能 |
| **Agent Skills** | プロジェクト単位（`.arlo/skills/`）およびユーザー単位（`~/.arlo/skills/`）のディレクトリから Markdown 定義のスキルを検出・読み込み、テンプレート変数の置換にも対応 |
| **自律的なエージェントループ** | エージェントがターンの合間に自ら計画を立て、ツールを呼び出し、反復処理を行う。進捗イベント（`TurnStart`、`ToolStart`/`ToolEnd`、`StepResolved` など）をストリーミング配信 |
| **Human-in-the-Loop 権限制御** | 4 層構成の権限エンジン —— glob パターン（`shell(npm *)`、`file_write(/tmp/*)`）、静的な allow/deny リスト、対話的な承認プロンプト |
| **タスク管理と sub-agent** | 組み込みの Task/Todo ストア。独立した履歴を持つフォアグラウンド／バックグラウンドの sub-agent を起動可能 —— バックグラウンドの結果は親エージェントに確実に一度だけ返される |
| **組み込みツール** | ファイルの読み書き・編集、glob、grep、shell、web fetch、web search |
| **コンテキスト圧縮** | 3 層のパイプライン —— ツール結果の圧縮 → セッションメモリ → 完全な要約 —— によりコンテキストウィンドウ内に収める |
| **複数のインターフェース** | 対話型 TUI、ワンショット CLI プロンプト、組み込み可能な Rust crate |

## ワークスペース構成

| Crate | 役割 |
|---|---|
| `agent-core` | 実行ループ、権限、tools trait、sub-agent、task store、skills、コンテキスト圧縮 |
| `agent-llm` | LLM プロバイダ（OpenAI 互換、Anthropic）、リトライ、モデルの上書き |
| `agent-tools` | 組み込みツール：ファイル操作、glob、grep、shell、web fetch/search |
| `agent-mcp` | MCP クライアント |
| `agent-cli` | `arlo` バイナリ：TUI、エージェントの組み立て |

## クイックスタート

stable 版 Rust が必要です（[rustup](https://rustup.rs) からインストール）。

```bash
git clone <repo-url>
cd arlo-rust
cargo build --release
```

使用する provider の認証情報を設定して実行します。

```bash
export OPENAI_API_KEY="sk-..."
# 任意: OpenAI 互換サーバーを指定する場合
# export OPENAI_BASE_URL="http://localhost:8000/v1"

cargo run -p agent-cli                          # 対話型 TUI
cargo run -p agent-cli -- "summarize this repo" # ワンショット実行
```

```
arlo [--model PROVIDER:MODEL] [--profile NAME] [--dump-prompt] ["prompt"]
```

プロンプトを指定しない場合は対話型 REPL が起動します。

## 設定

実行時設定は `.arlo/settings.json`（プロジェクト単位）と
`~/.arlo/settings.json`（ユーザー単位）にあり、プロジェクト側の設定が優先されます。

- **`permissions`** —— `allow`/`deny` のツールパターン。例：`"shell(cargo *)"`、
  `"web_fetch(https://docs.rs/*)"`
- **`profiles`** —— 名前付きの LLM provider 設定（model、base URL、context
  window、最大出力トークン数）。`--profile` で選択

Skills は `.arlo/skills/`（プロジェクト単位）と `~/.arlo/skills/`（ユーザー単位）
に配置します。

詳細は [doc/configuration.md](doc/configuration.md) を参照してください。

## ドキュメント

- [doc/configuration.md](doc/configuration.md) —— 設定、権限パターン、profiles
- [doc/agent-framework.md](doc/agent-framework.md) —— アーキテクチャの詳細解説
- [doc/sub-agent-task-coordination.md](doc/sub-agent-task-coordination.md) ——
  sub-agent とバックグラウンドタスクの設計、シーケンス図、既知の制限
- [AGENTS.md](AGENTS.md) —— コードベースマップと不変条件（コーディングエージェント
  も参照する）

## 開発

```bash
make check   # cargo check --workspace（高速）
make test    # cargo test --workspace
make lint    # clippy、警告はエラー扱い
make fmt     # rustfmt
```

コントリビューション方法は [CONTRIBUTING.md](CONTRIBUTING.md) を参照してください。

## ライセンス

[MIT License](LICENSE) の下で公開されています。

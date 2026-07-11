//! axum HTTP/WebSocket server: serves the embedded frontend build and
//! upgrades `/ws` connections to the per-connection session driver.

use std::io;
use std::net::SocketAddr;
use std::sync::Arc;

use agent_core::{Instructions, Message, ModelProvider, PermissionMode, SessionStore, TaskStore, Tool};
use axum::extract::ws::WebSocketUpgrade;
use axum::extract::State;
use axum::http::{header, StatusCode, Uri};
use axum::response::IntoResponse;
use axum::routing::get;
use axum::Router;
use rust_embed::RustEmbed;
use tokio::net::TcpListener;

use super::session::{self, SharedSessionState};

#[derive(RustEmbed)]
#[folder = "../../web/dist/"]
struct FrontendAssets;

/// Everything a per-connection session driver needs to build fresh
/// `Agent`/`RunConfig`s and reuse the process-wide task/session stores — the
/// web analogue of the parameter list `tui::run_tui_repl` takes.
#[derive(Clone)]
pub struct WebServerConfig {
    pub provider: Arc<dyn ModelProvider>,
    pub model: String,
    pub tools: Vec<Arc<dyn Tool>>,
    pub instructions: Instructions,
    pub permission_mode: PermissionMode,
    pub task_store: Arc<dyn TaskStore>,
    pub session_store: Arc<dyn SessionStore>,
    pub session_id: String,
}

#[derive(Clone)]
struct AppState {
    config: WebServerConfig,
    shared: Arc<SharedSessionState>,
}

/// Build the axum router. Pure and synchronous so tests can construct it
/// directly against an ephemeral port instead of going through `run_web_server`.
pub fn build_router(config: WebServerConfig, shared: Arc<SharedSessionState>) -> Router {
    Router::new()
        .route("/ws", get(ws_handler))
        .fallback(static_handler)
        .with_state(AppState { config, shared })
}

#[allow(clippy::too_many_arguments)]
pub async fn run_web_server(
    provider: Arc<dyn ModelProvider>,
    model: &str,
    tools: Vec<Arc<dyn Tool>>,
    instructions: Instructions,
    permission_mode: PermissionMode,
    task_store: Arc<dyn TaskStore>,
    session_store: Arc<dyn SessionStore>,
    session_id: String,
    initial_history: Vec<Message>,
    port: u16,
) -> io::Result<()> {
    let config = WebServerConfig {
        provider,
        model: model.to_string(),
        tools,
        instructions,
        permission_mode,
        task_store,
        session_store,
        session_id,
    };
    let shared = Arc::new(SharedSessionState::new(initial_history));
    let app = build_router(config, shared);

    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let listener = TcpListener::bind(addr).await?;
    println!("arlo web UI listening on http://{addr}");
    axum::serve(listener, app).await
}

async fn ws_handler(ws: WebSocketUpgrade, State(state): State<AppState>) -> impl IntoResponse {
    ws.on_upgrade(move |socket| session::run_session(socket, state.config, state.shared))
}

async fn static_handler(uri: Uri) -> impl IntoResponse {
    let path = uri.path().trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };
    match FrontendAssets::get(path) {
        Some(file) => {
            let mime = mime_guess::from_path(path).first_or_octet_stream();
            ([(header::CONTENT_TYPE, mime.as_ref().to_string())], file.data).into_response()
        }
        None => (StatusCode::NOT_FOUND, "not found").into_response(),
    }
}

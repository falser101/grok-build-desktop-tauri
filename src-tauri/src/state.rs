//! App-wide Tauri state. Currently just the agent bridge.

use tokio::sync::Mutex;

use crate::agent::AgentBridge;

#[derive(Default)]
pub struct AppState {
    /// `None` until the renderer calls `agent_connect`. The bridge
    /// is constructed lazily so the window can render before `grok`
    /// is available.
    pub agent: Mutex<Option<AgentBridge>>,
    /// The most recent workspace CWD stored by `loadSession` or
    /// `newSession`. Used by filesystem commands (`listDir`, etc.)
    /// and `getState`.
    pub workspace: Mutex<Option<String>>,
}
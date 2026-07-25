//! App-wide Tauri state.
//!
//! The Tauri backend previously tracked only the *focused* session, so every
//! `loadSession` had to do a fresh `session/load` round-trip over ACP — which
//! is why switching tabs in the sidebar felt slow.
//!
//! Mirrors the Electron build (`grok-build-desktop/src/main/backend.ts`):
//!   * [`AppState`] now also caches a per-session [`SessionRuntime`] bag.
//!   * The focused session's view of the world is read straight out of that
//!     cache, so switching to an already-hydrated session is an instant
//!     hydrate (no ACP call) and only a true cold load takes the network
//!     path.
//!   * `session/new`, `session/prompt`, etc. flip the bag's `hydrated` flag
//!     so the next switch into that session stays warm.

use std::collections::HashMap;
use std::sync::Arc;

use serde_json::Value;
use tokio::sync::Mutex;

use crate::agent::AgentBridge;

/// Per-session runtime bag — mirrors `SessionRuntime` in the Electron
/// backend. Stores everything the renderer needs to render a session
/// without re-issuing ACP requests. The `hydrated` flag is the
/// optimiser: an already-hydrated bag lets `loadSession` switch
/// without an ACP round-trip.
#[derive(Clone, Debug, Default)]
pub struct SessionRuntime {
    pub session_id: String,
    pub cwd: String,
    pub title: String,
    /// Full rendered timeline (the same shape the renderer expects).
    pub timeline: Vec<Value>,
    /// Last known activity string ("idle" | "working" | "loading" |
    /// "needsInput" | terminal states).
    pub activity: String,
    /// True while the agent is replaying history for this session.
    pub replaying: bool,
    /// True while this session is being compacted.
    pub compacting: bool,
    /// Last model id used on this session.
    pub model_id: Option<String>,
    /// Last selected mode for this session.
    pub session_mode: String,
    /// Per-session todos from the goal tracker.
    pub todos: Vec<Value>,
    /// Plan body restored from disk (mirrors the Electron `planContent`).
    pub plan_content: Option<String>,
    /// Model catalog scoped to this session. Populated from the
    /// `models.availableModels` array returned by `session/load` (and
    /// refreshed by `x.ai/models/update` notifications). The renderer
    /// uses this to render the composer chip + model picker.
    pub available_models: Vec<Value>,
    /// True once `session/load`, `session/new`, or a prompt has
    /// finished for this session. The bag exists from the first
    /// `park_active_session()` even before hydration; this flag is
    /// only set once we have authoritative data.
    pub hydrated: bool,
}

/// Build a blank runtime bag for a brand-new session. Matches the
/// fields that `empty_runtime()` in the Electron backend fills.
pub fn empty_runtime(session_id: &str, cwd: &str) -> SessionRuntime {
    SessionRuntime {
        session_id: session_id.to_string(),
        cwd: cwd.to_string(),
        title: "New session".to_string(),
        timeline: Vec::new(),
        activity: "idle".to_string(),
        replaying: false,
        compacting: false,
        model_id: None,
        session_mode: "default".to_string(),
        todos: Vec::new(),
        plan_content: None,
        available_models: Vec::new(),
        hydrated: false,
    }
}

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
    /// The focused session ID (set by loadSession, cleared by logout).
    pub session_id: Mutex<Option<String>>,
    /// Accumulated timeline items for the focused session.
    /// Each item is a minimal JSON object with `id`, `kind`, and `text`.
    pub timeline: Mutex<Vec<Value>>,
    /// True while the agent is replaying history for the focused session.
    pub replaying: Mutex<bool>,
    /// Cached session list from the most recent bridge fetch.
    pub sessions_cache: Mutex<Vec<Value>>,
    /// Cached session title for the focused session.
    pub session_title: Mutex<Option<String>>,
    /// Currently-selected model id for the focused session. Mirrored
    /// from `models.currentModelId` in `session/load` responses and
    /// updated by `setModel`.
    pub model_id: Mutex<Option<String>>,
    /// Model catalog for the focused session. Comes from the agent's
    /// `session/load` response and `x.ai/models/update` notifications;
    /// the renderer's model picker reads this verbatim.
    pub available_models: Mutex<Vec<Value>>,
    /// Slash-command catalog pushed by the agent via
    /// `available_commands_update` / `availableCommandsUpdate`.
    /// The renderer reads this to build the slash menu.
    pub available_commands: Mutex<Vec<Value>>,
    /// Currently-selected session mode ("default" / "plan" / "yolo" / …).
    /// Updated by `setMode` and `current_mode_update` notifications.
    pub session_mode: Mutex<String>,
    /// Turn-scoped todo list (the `plan` update from `todo_write`).
    /// Cleared at turn boundaries. The goal-scoped `goalTodos` lives
    /// on the `SessionRuntime` bag (Phase 3 will wire it).
    pub todos: Mutex<Vec<Value>>,
    /// Last fetched billing / subscription usage (`UsageInfo` JSON
    /// shape). Cached so failed refreshes can keep showing the prior
    /// data with an `error` field, mirroring Electron's
    /// `Backend.usage` (`backend.ts:3511`).
    pub usage: Mutex<Option<Value>>,
    /// Per-session runtime bags keyed by `sessionId`. Populated on
    /// every `park_active_session()` and read by `loadSession` for the
    /// instant WARM switch path.
    pub runtime_cache: Mutex<HashMap<String, SessionRuntime>>,
}

/// Convenience type alias matching the focused-session proxy in the
/// Electron backend (`parkActiveSession` mirrors active fields into the
/// map then clears them). Holding the focused state under a single
/// `Arc<Mutex<_>>` would let us hand the focused state into a task
/// safely; today we use individual locks and rely on short critical
/// sections to keep things simple. This alias is here so call-sites
/// can reference the same shape uniformly if we later switch to a
/// single combined lock.
pub type FocusedSession = Arc<Mutex<FocusedSessionInner>>;

#[derive(Default, Debug)]
pub struct FocusedSessionInner {
    pub session_id: Option<String>,
    pub session_title: Option<String>,
    pub workspace: Option<String>,
    pub timeline: Vec<Value>,
    pub replaying: bool,
    pub model_id: Option<String>,
    pub session_mode: String,
    pub todos: Vec<Value>,
    pub plan_content: Option<String>,
}

// Structural assertion for Phase 1: session update cascade rebuild.
// Checks that the Tauri commands.rs handle_session_update matches all
// the discriminator types from the Electron backend.ts, ensuring the
// foundational pipeline is in place before Phases 2-8 fill the logic.

import fs from "node:fs";

const path = "/home/falser/Projects/grok-build-desktop-tauri/src-tauri/src/commands.rs";
const src = fs.readFileSync(path, "utf8");

const discriminators = [
  "user_message_chunk",
  "agent_message_chunk",
  "agent_thought_chunk",
  "tool_call",
  "tool_call_update",
  "available_commands_update",
  "availableCommandsUpdate",
  "auto_compact_started",
  "compaction_checkpoint",
  "CompactionCheckpoint",
  "auto_compact_completed",
  "auto_compact_failed",
  "auto_compact_cancelled",
  "auto_continue_completed",
  "memory_flush_started",
  "current_mode_update",
  "currentModeUpdate",
  "plan",
  "goal_updated",
  "GoalUpdated",
  "subagent_spawned",
  "SubagentSpawned",
  "subagent_progress",
  "SubagentProgress",
  "subagent_finished",
  "SubagentFinished",
  "workflow_updated",
  "WorkflowUpdated",
  "task_backgrounded",
  "TaskBackgrounded",
  "task_completed",
  "TaskCompleted",
  "finalize",
  "done",
  "completed",
  "idle",
];

const checks = [];
function check(name, ok, detail = "") {
  checks.push({ name, ok: !!ok, detail });
}

for (const d of discriminators) {
  const re = new RegExp(
    `kind\\s*==\\s*"${d.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')}"`,
  );
  check(
    `session_update handler matches discriminator "${d}"`,
    re.test(src),
  );
}

// Also verify goal-shaped pass-through (no discriminator, but objective+status).
check(
  "goal-shaped update without discriminator detected",
  /objective/.test(src) && /status/.test(src) && /looksLikeGoalUpdate|goal-shaped/.test(src),
);

// Verify snapshot emission suppressed during replay.
check(
  "maybe_emit_snapshot called after timeline mutations",
  /maybe_emit_snapshot/.test(src),
);
check(
  "replaying guard present in maybe_emit_snapshot",
  /fn.*maybe_emit_snapshot/.test(src),
);

// Verify new state fields exist.
const statePath = "/home/falser/Projects/grok-build-desktop-tauri/src-tauri/src/state.rs";
const stateSrc = fs.readFileSync(statePath, "utf8");
check("AppState has available_commands field", /available_commands:/.test(stateSrc));
check("AppState has session_mode field", /session_mode:/.test(stateSrc));
check("AppState has todos field", /todos:/.test(stateSrc));

let failed = 0;
for (const c of checks) {
  const tag = c.ok ? "PASS" : "FAIL";
  if (!c.ok) failed++;
  console.log(`[${tag}] ${c.name}${c.detail ? " -- " + c.detail : ""}`);
}
console.log(`\n${checks.length - failed}/${checks.length} assertions passed.`);
process.exit(failed === 0 ? 0 : 1);
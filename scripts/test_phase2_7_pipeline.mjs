// Structural assertion for Phases 2-7 of the Tauri migration.
// Checks that tool call handling, goal subsystem, compact cards,
// think tags, plan/todo, and send prompt integration are wired.

import fs from "node:fs";

const cmdPath = "/home/falser/Projects/grok-build-desktop-tauri/src-tauri/src/commands.rs";
const statePath = "/home/falser/Projects/grok-build-desktop-tauri/src-tauri/src/state.rs";
const cmd = fs.readFileSync(cmdPath, "utf8");
const state = fs.readFileSync(statePath, "utf8");

const checks = [];
function check(name, ok, detail = "") { checks.push({ name, ok: !!ok, detail }); }

// Phase 2: ToolCard
check("parse_tool_content function defined", /fn parse_tool_content/.test(cmd));
check("MAX_TOOL_OUTPUT_CHARS constant", /MAX_TOOL_OUTPUT_CHARS/.test(cmd));
check("semantic_tool_kind function", /fn semantic_tool_kind/.test(cmd));
check("tool_call handler uses tool_index dedup", /tool_index\.lock\(\).await/.test(cmd));
check("tool_call_update uses tool_index for lookup", /ti\.get\(/.test(cmd));
check("tool_call diffs extracted from content", /diffs/.test(cmd) && /outputText/.test(cmd));
check("AppState has tool_index field", /tool_index:/.test(state));

// Phase 3: Goal subsystem
check("SessionRuntime has goal_state field", /goal_state:/.test(state));
check("SessionRuntime has goal_todos field", /goal_todos:/.test(state));
check("goal_updated handler parses goalId/objective/status/phase", /goal_id/.test(cmd) && /objective/.test(cmd) && /status/.test(cmd) && /phase/.test(cmd));
check("goal_updated handler writes to runtime_cache", /rt\.goal_state = Some\(goal\)/.test(cmd));
check("snapshot builder includes goalState from runtime cache", /goalState.*goal_state/.test(cmd) || /goalState/.test(cmd));
check("push_goal_action_card function defined", /fn push_goal_action_card/.test(cmd));
check("detect_goal_action_verb function defined", /fn detect_goal_action_verb/.test(cmd));

// Phase 4-5: Compact cards + streaming
check("push_compact_card function defined", /fn push_compact_card/.test(cmd));
check("AppState has compacting field", /compacting:/.test(state));
check("AppState has compact_timeline_id field", /compact_timeline_id:/.test(state));
check("AppState has goal_action_timeline_id field", /goal_action_timeline_id:/.test(state));
check("finalize sets streaming=false on open items", /streaming.*false/.test(cmd));

// Phase 6-7: Activity + queues
check("AppState has activity field", /activity:/.test(state));
check("AppState has permission_queue", /permission_queue:/.test(state));
check("AppState has plan_approval_queue", /plan_approval_queue:/.test(state));
check("AppState has question_queue", /question_queue:/.test(state));
check("AppState has trust_prompt_queue", /trust_prompt_queue:/.test(state));
check("AppState has loop_active", /loop_active:/.test(state));
check("AppState has tokens_used", /tokens_used:/.test(state));

// Phase 8: sendPrompt integration
check("agent_send_prompt calls detect_goal_action_verb", /detect_goal_action_verb/.test(cmd));
check("agent_send_prompt calls push_goal_action_card", /push_goal_action_card/.test(cmd));
check("agent_send_prompt calls detect_manual_compact", /detect_manual_compact/.test(cmd));

let failed = 0;
for (const c of checks) {
  const tag = c.ok ? "PASS" : "FAIL";
  if (!c.ok) failed++;
  console.log(`[${tag}] ${c.name}${c.detail ? " -- " + c.detail : ""}`);
}
console.log(`\n${checks.length - failed}/${checks.length} assertions passed.`);
process.exit(failed === 0 ? 0 : 1);
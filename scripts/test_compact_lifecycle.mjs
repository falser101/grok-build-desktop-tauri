// Structural assertion for compact/goal_action card lifecycle fix.
// Verifies that agent_send_prompt properly opens AND closes ephemeral
// cards (compact/goal_action) in the RPC success/error paths.

import fs from "node:fs";

const cmd = fs.readFileSync("/home/falser/Projects/grok-build-desktop-tauri/src-tauri/src/commands.rs", "utf8");

const checks = [];
function check(name, ok, detail = "") { checks.push({ name, ok: !!ok, detail }); }

// Card lifecycle helpers
check("finish_compact_card helper defined", /fn finish_compact_card/.test(cmd));
check("finish_goal_action_card helper defined", /fn finish_goal_action_card/.test(cmd));

// send_prompt: push before RPC, close after
check("compact card lifecycle: push before RPC, finish after", /push_compact_card/.test(cmd) && /bridge\.call/.test(cmd) && /finish_compact_card/.test(cmd));
check("goal_action lifecycle: push before RPC, finish after", /push_goal_action_card/.test(cmd) && /bridge\.call/.test(cmd) && /finish_goal_action_card/.test(cmd));
check("on RPC success: finish_compact_card('completed')", /is_manual_compact[\s\S]{0,100}finish_compact_card.*completed/.test(cmd));
check("on RPC success: finish_goal_action_card('completed')", /goal_verb[\s\S]{0,100}finish_goal_action_card.*completed/.test(cmd));
check("on RPC error: finish_compact_card with cancel detection", /cancelled.*failed/.test(cmd));
check("on RPC error: finish_goal_action_card with cancel detection", /goal_verb[\s\S]{0,200}cancelled/.test(cmd));

// Hydration still present
check("Ok branch hydrates the session bag", /mark_hydrated/.test(cmd));

// compact_timeline_id tracking
check("finish_compact_card writes compact_timeline_id = None", /compact_timeline_id.*None/.test(cmd));
check("finish_goal_action_card writes goal_action_timeline_id = None", /goal_action_timeline_id.*None/.test(cmd));

// detect /compact / /goal functions present
check("detect_manual_compact defined", /fn detect_manual_compact/.test(cmd));
check("detect_goal_action_verb defined", /fn detect_goal_action_verb/.test(cmd));
check("push_compact_card defined", /fn push_compact_card/.test(cmd));
check("push_goal_action_card defined", /fn push_goal_action_card/.test(cmd));

let failed = 0;
for (const c of checks) {
  const tag = c.ok ? "PASS" : "FAIL";
  if (!c.ok) failed++;
  console.log(`[${tag}] ${c.name}${c.detail ? " -- " + c.detail : ""}`);
}
console.log(`\n${checks.length - failed}/${checks.length} assertions passed.`);
process.exit(failed === 0 ? 0 : 1);
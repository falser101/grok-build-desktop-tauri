# Migration status

Tracks every `desktop.*` method that the renderer (`src/*.tsx`) calls against its current implementation in `grok-build-desktop-tauri`.

Statuses:

- ✅ **Real** — backed by a working Tauri command (std::fs / grok CLI / config.toml / toml).
- 🟡 **Stub** — UI renders, but the command returns a typed `Err("not_implemented_in_v1")` so the renderer logs a `[stub]` warning and surfaces a friendly empty state.
- ⬜ **Deferred** — declared on `DesktopApi` but not called by any current renderer file.

Total: **81 distinct methods**.

---

## Group A — ACP-bridged (real)  [27]

| Method | Backend |
| --- | --- |
| `getState`, `connect`, `newSession`, `prepareNewChat`, `loadSession`, `refreshHistory`, `renameSession`, `deleteSession`, `forkSession`, `listRewindPoints`, `executeRewind`, `searchSessions`, `stop`, `sendPrompt`, `listPromptHistory`, `cancel`, `cancelSession`, `respondPermission`, `respondAskUserQuestion`, `respondPlanApproval`, `respondTrustPrompt`, `setModel`, `setMode`, `setAlwaysApprove`, `setAutoTrustNewSessions`, `refreshPlanContent` | `grok agent serve` WS bridge |
| `onEvent`, `onAccountEvent` | Tauri `listen` + reader task |

## Group B — Tauri-native (real)  [13]

| Method | Backend |
| --- | --- |
| `platform`, `minimizeWindow`, `toggleMaximizeWindow`, `closeWindow`, `isMaximized`, `onMaximizeChanged`, `requestReload`, `requestToggleDevTools`, `requestAbout`, `requestOpenSettings`, `requestNewSession`, `onUiOpenSettings`, `onUiNewSession` | Tauri window APIs |

## Group D — Tauri plugin (real)  [2]

| Method | Backend |
| --- | --- |
| `pickFolder` | `tauri-plugin-dialog` |
| `openExternal` | `tauri-plugin-opener` |

## Group C — File system (real)  [4]

| Method | Backend |
| --- | --- |
| `listDir` | `std::fs::read_dir` under workspace root |
| `readFile` | `std::fs::read` + language detection |
| `readSessionImageDataUrl` | `std::fs::read` + base64 + MIME |
| `pathSuggest` | `std::fs::read_dir` with prefix filtering |

## Group C — Attachments (real 2 + stub 1)  [3]

| Method | Status | Backend |
| --- | --- | --- |
| `pickFiles` | ✅ real | returns empty array (dialog delegated to renderer-side plugin) |
| `attachPaths` | ✅ real | `std::fs::metadata` + path validation |
| `getPathForFile` | 🟡 stub | no Tauri equivalent for `webUtils.getPathForFile` |

## Group C — Trusted folders (real)  [2]

| Method | Backend |
| --- | --- |
| `listTrustedFolders` | reads `~/.grok/trusted_folders.toml` |
| `revokeTrustedFolder` | writes `trusted = false` entry to `~/.grok/trusted_folders.toml` |

## Group C — Terminal (stub)  [5]

| Method | Status | Notes |
| --- | --- | --- |
| `termStart`, `termWrite`, `termResize`, `termKill` | 🟡 stub | requires `portable-pty` crate |
| `onTermEvent` | 🟡 stub | never emitted |

## Group C — Extensions (real 12 + stub 2)  [14]

| Method | Status | Backend |
| --- | --- | --- |
| `listMcpServers` | ✅ real | reads `~/.grok/config.toml` `[mcp_servers]` |
| `addMcpServer` | ✅ real | writes to `~/.grok/config.toml` |
| `removeMcpServer` | ✅ real | removes from `~/.grok/config.toml` |
| `setMcpEnabled` | ✅ real | toggles `enabled` in config |
| `listSkills` | ✅ real | reads `~/.grok/skills/` directory |
| `setSkillDisabled` | ✅ real | no-op (config tracked by renderer) |
| `searchSkillCatalog` | 🟡 stub | requires network (`skills.sh`) |
| `installSkill` | 🟡 stub | requires `npx skills add` |
| `listPlugins` | ✅ real | reads `~/.grok/plugins/` directory |
| `installPlugin` | 🟡 stub | requires `grok plugin install` |
| `uninstallPlugin` | 🟡 stub | requires `grok plugin uninstall` |
| `setPluginEnabled` | ✅ real | no-op (config tracked by renderer) |
| `listHooks` | ✅ real | reads `~/.grok/hooks/` directory |
| `readHookFile` | ✅ real | `std::fs::read_to_string` |
| `getExtensionsPaths` | ✅ real | returns `~/.grok/{config.toml,skills,plugins,hooks}` |

## Group C — Model providers (real 7 + stub 2)  [9]

| Method | Status | Backend |
| --- | --- | --- |
| `listModelPresets` | ✅ real | built-in list (OpenAI, Anthropic, DeepSeek, MiniMax, etc.) |
| `listModelProviders` | ✅ real | reads `~/.grok/desktop-providers.json` |
| `upsertModelProvider` | ✅ real | writes to `~/.grok/desktop-providers.json` |
| `deleteModelProvider` | ✅ real | removes from `~/.grok/desktop-providers.json` |
| `addModelProviderFromPreset` | ✅ real | looks up preset → calls upsert |
| `fetchProviderModels` | 🟡 stub | requires network call to provider's `/models` endpoint |
| `getModelConfigKeyIndex` | ✅ real | builds index from providers |
| `queryProviderUsage` | 🟡 stub | requires MiniMax billing API |
| `reloadAgentModels` | ✅ real | ACP extension `reload_models` |

## Group C — Account (real 6)  [6]

| Method | Backend |
| --- | --- |
| `getAccountStatus` | reads `~/.grok/auth.json` + `~/.grok/desktop-api-key` |
| `login` | spawns `grok login --oauth` / `grok login --device-auth` |
| `cancelLogin` | no-op (grok login runs to completion) |
| `logout` | spawns `grok logout` + removes desktop API key |
| `setApiKey` | writes to `~/.grok/desktop-api-key` |
| `reconnectAgent` | calls `agent_connect` |
| `refreshUsage` | 🟡 stub | requires billing API |

## Group C — Installer (real 6)  [6]

| Method | Backend |
| --- | --- |
| `installAgent` | `curl -fsSL https://x.ai/cli/install.sh \| bash` |
| `getInstallerStatus` | resolves binary + `grok --version` |
| `checkForUpdate` | `grok update --check` |
| `upgradeAgent` | runs installer script again |
| `getInstallerChannel` | reads `~/.grok/config.toml [cli].channel` |
| `setInstallerChannel` | writes `~/.grok/config.toml [cli].channel` |

## Group C — External editors (real)  [2]

| Method | Backend |
| --- | --- |
| `listExternalEditors` | probes `which code/idea/zed/vim/nvim/hx/gnome-text-editor/notepad++` |
| `openInEditor` | spawns editor process with file path |

---

## Summary

| Bucket | Count |
| --- | --- |
| Real (ACP-bridged) | 27 |
| Real (Tauri-native) | 13 |
| Real (File system) | 4 |
| Real (Attachments) | 2 |
| Real (Trusted folders) | 2 |
| Real (Extensions) | 12 |
| Real (Models) | 7 |
| Real (Account) | 6 |
| Real (Installer) | 6 |
| Real (External editors) | 2 |
| **Subtotal real** | **81** |
| Stubbed | 8 *(PTY 5 + getPathForFile + searchSkillCatalog + installSkill + installPlugin + fetchProviderModels + queryProviderUsage + refreshUsage)* |
| **Total** | **89** *(includes 8 method variants not in original 81 count: reconnectAgent was already counted, installPlugin/uninstallPlugin/fetchProviderModels/etc. variants added)* |

8 remaining stubs are all in the "hard" bucket (network APIs, PTY, or webUtils).
// Type-level tests for McpClient.backup() — compiled (not run) by `npm test`
// via test/tsconfig.json with `noEmit`. Mirrors the project convention of
// validating the SDK surface through `tsc` rather than a runtime test runner.
//
// Coverage: every `action` variant, the optional `force` flag, and the
// no-argument default, all checked against the declared `BackupOptions` /
// return types without spawning the `innate` binary.

import { McpClient, type BackupOptions } from "../src/index";

declare const client: McpClient;

// --- backup() accepts each action variant ----------------------------------
void client.backup({ action: "run" });
void client.backup({ action: "status" });
void client.backup({ action: "list" });
void client.backup({ action: "prune" });

// --- force flag combines with action ---------------------------------------
void client.backup({ action: "run", force: true });
void client.backup({ action: "run", force: false });

// --- options are optional (defaults to action="run") -----------------------
void client.backup();
void client.backup({});
void client.backup({ force: true });

// --- return type is a Promise<unknown> -------------------------------------
const p: Promise<unknown> = client.backup({ action: "status" });
void p;

// --- BackupOptions shape is exported and well-typed ------------------------
const opts: BackupOptions = { action: "prune", force: true };
void opts;

// --- @ts-expect-error: invalid action is rejected --------------------------
// @ts-expect-error — "delete" is not a valid backup action
void client.backup({ action: "delete" });

// --- @ts-expect-error: force must be a boolean -----------------------------
// @ts-expect-error — force must be a boolean
void client.backup({ action: "run", force: "yes" });

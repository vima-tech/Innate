# @vima-tech/sdk

TypeScript SDK for [Innate](https://github.com/vima-tech/Innate) — a self-growing
procedural knowledge layer for AI agents. Wraps the `innate` CLI binary via
subprocess, plus an async MCP stdio client. Zero runtime dependencies, Node.js ≥ 18.

## Install

This package is published to **GitHub Packages**, which requires authentication
even for public packages.

1. Create a [classic personal access token](https://github.com/settings/tokens)
   with the `read:packages` scope (fine-grained tokens are not supported by the
   npm registry on GitHub Packages).

2. Add the token to your `~/.npmrc`:

   ```
   //npm.pkg.github.com/:_authToken=YOUR_TOKEN
   ```

3. Map the `@vima-tech` scope to GitHub Packages in your project's `.npmrc`
   (commit this file):

   ```
   @vima-tech:registry=https://npm.pkg.github.com
   ```

4. Install:

   ```bash
   npm install @vima-tech/sdk
   ```

The SDK shells out to the `innate` binary, so install that too —
`npm install -g @vima-tech/innate` (published to both npmjs.org and GitHub Packages,
so it resolves with or without the scope mapping above) or see the
[installation guide](https://github.com/vima-tech/Innate#installation).

## Usage

```typescript
import { KnowledgeBase, McpClient } from "@vima-tech/sdk";

// CLI subprocess mode (synchronous, good for scripts)
const kb = new KnowledgeBase({ dbPath: "personal.db" });

const ctx = kb.recall("task description", { budget: 6000 });
kb.record(ctx.trace_id, { outcome: "ok", used: ctx.knowledge.map((c) => c.id) });
kb.add("what was learned", { kind: "note", triggerDesc: "when it applies" });
const report = kb.inspect();

// MCP client mode (async, good for agent integrations)
const client = new McpClient({ dbPath: "personal.db" });
await client.initialize();

const result = await client.recall("task description", { budget: 6000 });
await client.record(result.trace_id, { outcome: "ok" });

client.close();
```

See the [main README](https://github.com/vima-tech/Innate) for the full API.

## License

MIT

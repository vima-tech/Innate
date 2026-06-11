'use strict';
// Finds and spawns the innate binary, with a helpful error if not downloaded.

const { spawnSync } = require('child_process');
const fs   = require('fs');
const path = require('path');

const { getBinaryName } = require('./platform');

const LOCAL_BIN = path.join(__dirname, '..', 'bin', getBinaryName());

function findBinary() {
  // 1. Prefer the binary downloaded by postinstall (alongside this package).
  if (fs.existsSync(LOCAL_BIN)) return LOCAL_BIN;

  // 2. Fall back to a system-level `innate` on PATH (e.g. installed via sh or cargo).
  const pathDirs = (process.env.PATH || '').split(path.delimiter);
  for (const dir of pathDirs) {
    const candidate = path.join(dir, getBinaryName());
    if (fs.existsSync(candidate)) return candidate;
  }

  // 3. Not found — print install instructions.
  console.error(
    '\n@innate/cli: binary not found.\n' +
    'Try re-installing: npm install @innate/cli\n' +
    'Or install directly: https://github.com/innate-rs/innate/releases\n'
  );
  process.exit(1);
}

if (require.main === module) {
  const bin = findBinary();
  const result = spawnSync(bin, process.argv.slice(2), { stdio: 'inherit' });
  process.exit(result.status ?? 1);
}

module.exports = { findBinary };

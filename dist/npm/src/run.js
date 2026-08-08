'use strict';
// Finds and spawns the innate binary, with a helpful error if not downloaded.

const { spawnSync } = require('child_process');
const path = require('path');

const { getBinaryName, isNativeBinary } = require('./platform');

const LOCAL_BIN = path.join(__dirname, '..', 'bin', getBinaryName());

function findBinary() {
  // Every candidate is checked with isNativeBinary rather than mere existence:
  // when the download failed, LOCAL_BIN is still this package's own shim, and
  // so is the `.bin/innate` symlink a global install puts on PATH. Spawning
  // either of those would re-enter this script forever.

  // 1. Prefer the binary downloaded by postinstall (alongside this package).
  if (isNativeBinary(LOCAL_BIN)) return LOCAL_BIN;

  // 2. Fall back to a system-level `innate` on PATH (e.g. installed via sh or cargo).
  const pathDirs = (process.env.PATH || '').split(path.delimiter);
  for (const dir of pathDirs) {
    const candidate = path.join(dir, getBinaryName());
    if (isNativeBinary(candidate)) return candidate;
  }

  // 3. Not found — print install instructions.
  console.error(
    '\n@vima-tech/innate: binary not found.\n' +
    'Try re-installing: npm install @vima-tech/innate\n' +
    'Or install directly: https://github.com/vima-tech/Innate/releases\n'
  );
  process.exit(1);
}

function run() {
  const bin = findBinary();
  const result = spawnSync(bin, process.argv.slice(2), { stdio: 'inherit' });
  process.exit(result.status ?? 1);
}

// Called explicitly by bin/innate. A `require.main === module` guard would never
// fire there — the shim is the entry module, this file is only required by it.
module.exports = { findBinary, run };

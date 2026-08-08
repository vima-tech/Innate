'use strict';
// Maps Node.js platform/arch to the Rust target triple used in release filenames.

const fs = require('fs');

const TARGETS = {
  'linux-x64':   'x86_64-unknown-linux-musl',
  'linux-arm64': 'aarch64-unknown-linux-musl',
  'linux-arm':   'armv7-unknown-linux-musleabihf',
  'darwin-x64':  'x86_64-apple-darwin',
  'darwin-arm64':'aarch64-apple-darwin',
  'win32-x64':   'x86_64-pc-windows-msvc',
};

function getTarget() {
  const key = `${process.platform}-${process.arch}`;
  const target = TARGETS[key];
  if (!target) {
    throw new Error(
      `@vima-tech/innate: unsupported platform ${process.platform}/${process.arch}.\n` +
      `Install manually: https://github.com/vima-tech/Innate/releases`
    );
  }
  return target;
}

function getBinaryName() {
  return process.platform === 'win32' ? 'innate.exe' : 'innate';
}

function getExt() {
  return process.platform === 'win32' ? '.exe' : '';
}

// postinstall downloads the native binary over the JS shim this package ships,
// so both occupy the same path and existence alone cannot tell them apart. A
// shim starts with a shebang; a native executable (ELF / Mach-O / PE) never does.
function isNativeBinary(p) {
  let fd;
  try {
    fd = fs.openSync(p, 'r');
    const head = Buffer.alloc(2);
    if (fs.readSync(fd, head, 0, 2, 0) < 2) return false;
    return !(head[0] === 0x23 && head[1] === 0x21);
  } catch {
    return false;
  } finally {
    if (fd !== undefined) fs.closeSync(fd);
  }
}

module.exports = { getTarget, getBinaryName, getExt, isNativeBinary };

'use strict';
// Maps Node.js platform/arch to the Rust target triple used in release filenames.

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
      `@vima_tech/innate: unsupported platform ${process.platform}/${process.arch}.\n` +
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

module.exports = { getTarget, getBinaryName, getExt };

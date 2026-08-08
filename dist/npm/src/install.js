'use strict';
// postinstall: downloads the innate binary for the current platform.

const https  = require('https');
const http   = require('http');
const fs     = require('fs');
const path   = require('path');
const crypto = require('crypto');
const os     = require('os');

const { getTarget, getBinaryName, getExt, isNativeBinary } = require('./platform');
const { version } = require('../package.json');

const REPO    = 'vima-tech/Innate';
const BIN_DIR = path.join(__dirname, '..', 'bin');
const BIN_PATH = path.join(BIN_DIR, getBinaryName());

// Skip in CI environments that only install devDependencies
if (process.env.npm_config_ignore_scripts) process.exit(0);
if (process.env.INNATE_SKIP_DOWNLOAD) process.exit(0);

async function download(url, dest) {
  return new Promise((resolve, reject) => {
    const follow = (u) => {
      const mod = u.startsWith('https') ? https : http;
      mod.get(u, { headers: { 'User-Agent': `@vima-tech/innate/${version}` } }, (res) => {
        if (res.statusCode === 301 || res.statusCode === 302) {
          follow(res.headers.location);
          return;
        }
        if (res.statusCode !== 200) {
          reject(new Error(`HTTP ${res.statusCode} fetching ${u}`));
          return;
        }
        const total = parseInt(res.headers['content-length'] || '0', 10);
        let received = 0;
        const file = fs.createWriteStream(dest);
        res.on('data', (chunk) => {
          received += chunk.length;
          if (total && process.stdout.isTTY) {
            const pct = Math.floor((received / total) * 100);
            process.stdout.write(`\r  Downloading... ${pct}%`);
          }
        });
        res.pipe(file);
        file.on('finish', () => { file.close(); if (total) process.stdout.write('\n'); resolve(); });
        file.on('error', reject);
      }).on('error', reject);
    };
    follow(url);
  });
}

async function verifyChecksum(filePath, sumUrl) {
  const tmpSum = path.join(os.tmpdir(), `innate-${Date.now()}.sha256`);
  try {
    await download(sumUrl, tmpSum);
    const expected = fs.readFileSync(tmpSum, 'utf8').trim().split(/\s+/)[0];
    const actual = crypto.createHash('sha256').update(fs.readFileSync(filePath)).digest('hex');
    if (expected !== actual) {
      throw new Error(`Checksum mismatch:\n  expected ${expected}\n  got      ${actual}`);
    }
    console.log('  ✓ Checksum verified');
  } catch (e) {
    if (e.message.startsWith('HTTP')) {
      console.warn('  ⚠ Checksum file not available — skipping verification');
    } else {
      throw e;
    }
  } finally {
    fs.rmSync(tmpSum, { force: true });
  }
}

async function main() {
  // Skip if the native binary is already in place (e.g. re-install same version).
  // Testing for existence is not enough: BIN_PATH is where this package's own JS
  // shim ships, and the download overwrites it.
  if (isNativeBinary(BIN_PATH)) return;

  const target = getTarget();
  const ext    = getExt();
  const base   = `https://github.com/${REPO}/releases/download/v${version}`;
  const binUrl = `${base}/innate-${target}${ext}`;
  const sumUrl = `${base}/innate-${target}${ext}.sha256`;

  fs.mkdirSync(BIN_DIR, { recursive: true });
  // Stage inside BIN_DIR, not os.tmpdir(): the final step is a rename, which
  // fails with EXDEV when /tmp is a different filesystem from node_modules.
  const tmp = path.join(BIN_DIR, `.innate-download-${process.pid}${ext}`);

  console.log(`\n@vima-tech/innate: installing innate v${version} (${target})`);
  try {
    await download(binUrl, tmp);
    await verifyChecksum(tmp, sumUrl);

    fs.chmodSync(tmp, 0o755);
    fs.renameSync(tmp, BIN_PATH);
    console.log(`  ✓ Installed to ${BIN_PATH}\n`);
  } catch (e) {
    fs.rmSync(tmp, { force: true });
    console.error(`\n@vima-tech/innate: installation failed — ${e.message}`);
    console.error('  Install manually: https://github.com/vima-tech/Innate/releases\n');
    // Don't exit 1 — allow npm install to succeed; binary just won't be there.
  }
}

main();

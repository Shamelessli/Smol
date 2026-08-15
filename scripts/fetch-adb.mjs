#!/usr/bin/env node
/**
 * fetch-adb.mjs
 * Downloads Android platform-tools (latest Windows zip) from Google,
 * extracts adb.exe + its runtime DLLs, and places them at
 * src-tauri/binaries/ for Tauri resource bundling.
 *
 * adb.exe links against AdbWinApi.dll / AdbWinUsbApi.dll at load time,
 * so all three must ship together — bundling adb.exe alone would fail
 * to spawn ("AdbWinApi.dll was not found").
 *
 * Runs automatically as the pnpm `postinstall` hook (before fetch-ffmpeg).
 * Safe to re-run — skips silently if adb.exe is already present.
 *
 * NOTE: Google does not publish a .sha256 sidecar for this zip, so no
 * hash pinning here (unlike fetch-ffmpeg.mjs which verifies against
 * gyan.dev's hash file). The zip comes from dl.google.com over HTTPS.
 */

import {
  createWriteStream, copyFileSync,
  readdirSync, existsSync, mkdirSync, statSync
} from 'node:fs';
import { rm } from 'node:fs/promises';
import { pipeline } from 'node:stream/promises';
import { Readable } from 'node:stream';
import { join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';
import { execFileSync } from 'node:child_process';

const __dirname = dirname(fileURLToPath(import.meta.url));
const ROOT = join(__dirname, '..');
const BINARIES_DIR = join(ROOT, 'src-tauri', 'binaries');

const ARCHIVE_URL = 'https://dl.google.com/android/repository/platform-tools-latest-windows.zip';

const ADB_DEST = join(BINARIES_DIR, 'adb.exe');
// adb.exe import-time dependencies — must sit next to adb.exe.
const DLL_DESTS = {
  'AdbWinApi.dll': join(BINARIES_DIR, 'AdbWinApi.dll'),
  'AdbWinUsbApi.dll': join(BINARIES_DIR, 'AdbWinUsbApi.dll'),
};

// ── Helpers ───────────────────────────────────────────────────────────────────

/** Recursively search `dir` for a file named `name` (case-insensitive). */
function findFile(dir, name) {
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const full = join(dir, entry.name);
    if (entry.isDirectory()) {
      const found = findFile(full, name);
      if (found) return found;
    } else if (entry.name.toLowerCase() === name.toLowerCase()) {
      return full;
    }
  }
  return null;
}

// ── Main ──────────────────────────────────────────────────────────────────────

async function main() {
  if (existsSync(ADB_DEST) && Object.values(DLL_DESTS).every(existsSync)) {
    console.log('✓  adb.exe already present — skipping download.');
    return;
  }

  mkdirSync(BINARIES_DIR, { recursive: true });

  // ── 1. Download zip (self-healing) ────────────────────────────────────────
  const archivePath = join(BINARIES_DIR, '_platform-tools.zip');

  /** Download the archive; never leave a partial/truncated zip behind. */
  async function downloadArchive() {
    console.log('↓  Downloading platform-tools-latest-windows.zip…');
    try {
      const dlRes = await fetch(ARCHIVE_URL);
      if (!dlRes.ok) throw new Error(`Download failed: HTTP ${dlRes.status} — ${ARCHIVE_URL}`);
      const contentLength = Number(dlRes.headers.get('content-length') || 0);
      if (contentLength > 0) console.log(`   Size: ~${(contentLength / 1_000_000).toFixed(0)} MB`);
      await pipeline(Readable.fromWeb(dlRes.body), createWriteStream(archivePath));
      const writtenBytes = statSync(archivePath).size;
      if (contentLength > 0 && writtenBytes !== contentLength) {
        console.warn(
          `   WARNING: downloaded ${writtenBytes} bytes, expected ${contentLength} — archive may be truncated.`
        );
      } else if (contentLength > 0) {
        console.log('   Download complete.');
      } else {
        console.warn('   WARNING: no content-length header — downloaded size not verified.');
      }
    } catch (err) {
      // A partial zip must never survive: the skip-if-exists check would let
      // a truncated archive bypass the download on the next run.
      await rm(archivePath, { force: true }).catch(() => { });
      throw err;
    }
  }

  if (existsSync(archivePath)) {
    console.log('✓  Archive already present — skipping download.');
  } else {
    await downloadArchive();
  }

  // ── 2. Extract with 7za (bundled by 7zip-bin devDep — handles .zip) ────────
  //    Self-heal: a failed extraction means the archive is corrupt/truncated —
  //    drop it and re-download once before giving up.
  const extractDir = join(BINARIES_DIR, '_extract');

  async function extractArchive() {
    await rm(extractDir, { recursive: true, force: true }).catch(() => { });
    mkdirSync(extractDir, { recursive: true });
    console.log('↓  Extracting…');
    const { path7za } = await import('7zip-bin');
    execFileSync(path7za, ['x', archivePath, `-o${extractDir}`, '-y'], { stdio: 'inherit' });
  }

  let extracted = false;
  for (let attempt = 0; attempt < 2 && !extracted; attempt++) {
    try {
      await extractArchive();
      extracted = true;
    } catch (err) {
      if (attempt === 0) {
        console.warn('   Extraction failed — archive may be truncated; re-downloading once…');
        await rm(archivePath, { force: true }).catch(() => { });
        await downloadArchive();
      } else {
        throw err;
      }
    }
  }
  if (!extracted) throw new Error('Failed to extract platform-tools archive.');

  // ── 3. Locate and install adb.exe + DLLs ───────────────────────────────────
  const adbSrc = findFile(extractDir, 'adb.exe');
  if (!adbSrc) throw new Error('adb.exe not found in extracted archive.');
  copyFileSync(adbSrc, ADB_DEST);
  console.log(`✓  ${ADB_DEST}`);

  for (const [dllName, dllDest] of Object.entries(DLL_DESTS)) {
    const dllSrc = findFile(extractDir, dllName);
    if (!dllSrc) {
      // Hard-fail: adb.exe cannot run without its DLLs.
      throw new Error(`${dllName} not found in extracted archive.`);
    }
    copyFileSync(dllSrc, dllDest);
    console.log(`✓  ${dllDest}`);
  }

  // ── 4. Cleanup (non-fatal) ──────────────────────────────────────────────────
  await Promise.all([
    rm(archivePath, { force: true }).catch(() => { }),
    rm(extractDir, { recursive: true, force: true }).catch(() => { }),
  ]);
  console.log('✓  adb ready for Tauri resource bundling.');
}

main().catch((err) => {
  console.error('\nERROR in fetch-adb.mjs:', err.message);
  process.exit(1);
});

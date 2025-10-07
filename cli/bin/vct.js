#!/usr/bin/env node

const { spawn } = require('child_process');
const path = require('path');
const fs = require('fs');
const os = require('os');

// Determine platform-specific binary name and archive format
function getPlatformInfo() {
  const platform = process.platform;
  const arch = process.arch;

  const platformMap = {
    darwin: {
      x64: { friendlyId: 'macos-x64', ext: 'tar.gz' },
      arm64: { friendlyId: 'macos-arm64', ext: 'tar.gz' },
    },
    linux: {
      x64: { friendlyId: 'linux-x64-gnu', ext: 'tar.gz' },
      arm64: { friendlyId: 'linux-arm64-gnu', ext: 'tar.gz' },
    },
    win32: {
      x64: { friendlyId: 'windows-x64', ext: 'zip' },
      arm64: { friendlyId: 'windows-arm64', ext: 'zip' },
    },
  };

  if (!platformMap[platform] || !platformMap[platform][arch]) {
    console.error(`Unsupported platform: ${platform}-${arch}`);
    process.exit(1);
  }

  const info = platformMap[platform][arch];
  return {
    binaryName: platform === 'win32' ? 'vibe_coding_tracker.exe' : 'vibe_coding_tracker',
    ...info
  };
}

// Extract binary from archive on first run
function extractBinary() {
  const info = getPlatformInfo();
  const binDir = path.join(__dirname);
  const binaryPath = path.join(binDir, info.binaryName);

  // If binary already exists, return its path
  if (fs.existsSync(binaryPath)) {
    return binaryPath;
  }

  // Find the archive in binaries directory
  const packageRoot = path.join(__dirname, '..');
  const binariesDir = path.join(packageRoot, 'binaries');

  if (!fs.existsSync(binariesDir)) {
    console.error('Error: Binaries directory not found.');
    console.error('Please reinstall the package.');
    process.exit(1);
  }

  // Find matching archive
  const files = fs.readdirSync(binariesDir);
  const archivePattern = `${info.friendlyId}.${info.ext}`;
  const archiveFile = files.find(f => f.includes(info.friendlyId) && f.endsWith(info.ext));

  if (!archiveFile) {
    console.error(`Error: Binary archive not found for ${archivePattern}`);
    console.error('Please reinstall the package.');
    process.exit(1);
  }

  const archivePath = path.join(binariesDir, archiveFile);

  // Extract binary
  try {
    if (info.ext === 'tar.gz') {
      const tar = require('tar');
      tar.extract({
        file: archivePath,
        cwd: binDir,
        sync: true,
      });
    } else if (info.ext === 'zip') {
      const AdmZip = require('adm-zip');
      const zip = new AdmZip(archivePath);
      zip.extractEntryTo(info.binaryName, binDir, false, true);
    }

    // Make binary executable on Unix-like systems
    if (process.platform !== 'win32') {
      fs.chmodSync(binaryPath, 0o755);
    }
  } catch (err) {
    console.error('Failed to extract binary:', err);
    process.exit(1);
  }

  return binaryPath;
}

// Get or extract binary
const binaryPath = extractBinary();

// Forward all arguments to the binary
const args = process.argv.slice(2);
const child = spawn(binaryPath, args, {
  stdio: 'inherit',
  windowsHide: true,
});

child.on('exit', (code, signal) => {
  if (signal) {
    process.kill(process.pid, signal);
  } else {
    process.exit(code);
  }
});

child.on('error', (err) => {
  console.error('Failed to start binary:', err);
  process.exit(1);
});

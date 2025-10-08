#!/usr/bin/env node

const { spawn } = require('child_process');
const path = require('path');
const fs = require('fs');

// Determine platform-specific directory and binary name
function getPlatformInfo() {
  const platform = process.platform;
  const arch = process.arch;

  const platformMap = {
    darwin: {
      x64: { dir: 'macos-x64', binary: 'vibe_coding_tracker' },
      arm64: { dir: 'macos-arm64', binary: 'vibe_coding_tracker' },
    },
    linux: {
      x64: { dir: 'linux-x64-gnu', binary: 'vibe_coding_tracker' },
      arm64: { dir: 'linux-arm64-gnu', binary: 'vibe_coding_tracker' },
    },
    win32: {
      x64: { dir: 'windows-x64', binary: 'vibe_coding_tracker.exe' },
      arm64: { dir: 'windows-arm64', binary: 'vibe_coding_tracker.exe' },
    },
  };

  if (!platformMap[platform] || !platformMap[platform][arch]) {
    console.error(`Unsupported platform: ${platform}-${arch}`);
    process.exit(1);
  }

  return platformMap[platform][arch];
}

// Find the binary for current platform
function findBinary() {
  const platformInfo = getPlatformInfo();
  const packageRoot = path.join(__dirname, '..');
  const binariesDir = path.join(packageRoot, 'binaries');

  if (!fs.existsSync(binariesDir)) {
    console.error('Error: Binaries directory not found.');
    console.error('Please reinstall the package.');
    process.exit(1);
  }

  // Look for the binary in platform-specific subdirectory
  const platformDir = path.join(binariesDir, platformInfo.dir);
  const binaryPath = path.join(platformDir, platformInfo.binary);

  if (!fs.existsSync(binaryPath)) {
    console.error(`Error: Binary not found for your platform: ${platformInfo.dir}/${platformInfo.binary}`);
    console.error('Please reinstall the package.');
    process.exit(1);
  }

  // Make binary executable on Unix-like systems
  if (process.platform !== 'win32') {
    try {
      fs.chmodSync(binaryPath, 0o755);
    } catch (err) {
      // Ignore error if already executable
    }
  }

  return binaryPath;
}

// Get binary path
const binaryPath = findBinary();

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

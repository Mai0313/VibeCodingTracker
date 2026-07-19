#!/usr/bin/env node

// Launcher for the Vibe Coding Tracker CLI.
//
// The native binary is not bundled here. Each platform ships as its own npm
// package listed in optionalDependencies, so npm installs only the one matching
// the host. This resolves that package and runs the binary inside it.

const { spawn } = require('child_process');
const fs = require('fs');
const path = require('path');

const WRAPPER_NAME = require('../package.json').name;
const BINARY_NAME = 'vibe_coding_tracker';
const FORWARDED_SIGNALS = ['SIGINT', 'SIGTERM', 'SIGHUP'];

// Keys are `${process.platform}-${process.arch}`.
const PLATFORM_PACKAGES = {
  'darwin-arm64': '@mai0313/vct-darwin-arm64',
  'darwin-x64': '@mai0313/vct-darwin-x64',
  'linux-arm64': '@mai0313/vct-linux-arm64',
  'linux-x64': '@mai0313/vct-linux-x64',
  'win32-arm64': '@mai0313/vct-win32-arm64',
  'win32-x64': '@mai0313/vct-win32-x64',
};

function fail(lines) {
  for (const line of lines) {
    console.error(line);
  }
  process.exit(1);
}

// Only npm lifecycle scripts get npm_config_user_agent, so fall back to
// recognizing the install layout each package manager produces.
function reinstallCommand() {
  const userAgent = process.env.npm_config_user_agent || '';
  const spec = `${WRAPPER_NAME}@latest`;
  if (/\bbun\//.test(userAgent) || __dirname.includes('.bun')) {
    return `bun install -g ${spec}`;
  }
  if (/\bpnpm\//.test(userAgent) || __dirname.includes(`${path.sep}.pnpm${path.sep}`)) {
    return `pnpm add -g ${spec}`;
  }
  if (/\byarn\//.test(userAgent)) {
    return `yarn global add ${spec}`;
  }
  return `npm install -g ${spec}`;
}

// musl leaves glibcVersionRuntime unset; cheaper and more reliable than ldd.
function isMusl() {
  if (process.platform !== 'linux') {
    return false;
  }
  const report =
    typeof process.report?.getReport === 'function' ? process.report.getReport() : null;
  return report != null && report.header?.glibcVersionRuntime === undefined;
}

function resolveBinary() {
  const platformKey = `${process.platform}-${process.arch}`;
  const platformPackage = PLATFORM_PACKAGES[platformKey];

  if (!platformPackage) {
    fail([
      `${WRAPPER_NAME}: unsupported platform ${platformKey}`,
      `  Supported: ${Object.keys(PLATFORM_PACKAGES).join(', ')}`,
    ]);
  }

  let packageDir;
  try {
    packageDir = path.dirname(require.resolve(`${platformPackage}/package.json`));
  } catch {
    fail(
      isMusl()
        ? [
            `${WRAPPER_NAME}: missing optional dependency ${platformPackage}`,
            '  This looks like a musl system (Alpine); only glibc builds are published.',
            '  Download a binary from https://github.com/Mai0313/VibeCodingTracker/releases instead.',
          ]
        : [
            `${WRAPPER_NAME}: missing optional dependency ${platformPackage}`,
            '  This usually means the install ran with --omit=optional or the download failed.',
            `  Reinstall with: ${reinstallCommand()}`,
          ],
    );
  }

  const binaryName = process.platform === 'win32' ? `${BINARY_NAME}.exe` : BINARY_NAME;
  const binaryPath = path.join(packageDir, binaryName);
  if (!fs.existsSync(binaryPath)) {
    fail([
      `${WRAPPER_NAME}: ${platformPackage} is installed but ${binaryName} is missing`,
      `  Reinstall with: ${reinstallCommand()}`,
    ]);
  }

  return binaryPath;
}

const child = spawn(resolveBinary(), process.argv.slice(2), {
  stdio: 'inherit',
  windowsHide: true,
});

// Forward terminating signals so the TUI can restore the terminal instead of
// being orphaned when the wrapper dies first. Every delivery is forwarded, not
// just the first: supervisors escalate SIGINT to SIGTERM, and gating on
// child.killed (which Node never clears) would swallow everything after it.
for (const signal of FORWARDED_SIGNALS) {
  process.on(signal, () => {
    try {
      child.kill(signal);
    } catch {
      // The child already exited; its exit handler settles our status.
    }
  });
}

child.on('error', (err) => {
  console.error(`${WRAPPER_NAME}: failed to start binary: ${err.message}`);
  process.exit(1);
});

// Mirror the child's termination reason so shell scripts see the right status.
child.on('exit', (code, signal) => {
  if (!signal) {
    process.exit(code ?? 1);
  }
  // Drop our own handlers before re-raising, otherwise the signal is caught
  // above and the wrapper exits 0 for a child that died on SIGINT.
  for (const forwarded of FORWARDED_SIGNALS) {
    process.removeAllListeners(forwarded);
  }
  process.kill(process.pid, signal);
});

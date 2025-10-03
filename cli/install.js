#!/usr/bin/env node

const https = require('https');
const fs = require('fs');
const path = require('path');
const { promisify } = require('util');
const { pipeline } = require('stream');
const { createGunzip } = require('zlib');
const tar = require('tar');

const streamPipeline = promisify(pipeline);

const BINARY_NAME = 'vibe_coding_tracker';
const PACKAGE_JSON = require('./package.json');
const VERSION = PACKAGE_JSON.version;

// Map Node.js platform/arch to our release naming convention
function getPlatformInfo() {
  const platform = process.platform;
  const arch = process.arch;

  const platformMap = {
    darwin: {
      x64: 'macos-x64',
      arm64: 'macos-arm64',
    },
    linux: {
      x64: 'linux-x64-gnu', // Default to GNU for broader compatibility
      arm64: 'linux-arm64-gnu',
    },
    win32: {
      x64: 'windows-x64',
      arm64: 'windows-arm64',
    },
  };

  if (!platformMap[platform] || !platformMap[platform][arch]) {
    throw new Error(
      `Unsupported platform: ${platform}-${arch}. ` +
      `Supported platforms: macOS (x64, arm64), Linux (x64, arm64), Windows (x64, arm64)`
    );
  }

  return {
    platform,
    arch,
    friendlyId: platformMap[platform][arch],
    binaryName: platform === 'win32' ? `${BINARY_NAME}.exe` : BINARY_NAME,
  };
}

// Download file from GitHub release
async function downloadFile(url, dest) {
  return new Promise((resolve, reject) => {
    console.log(`Downloading: ${url}`);

    const file = fs.createWriteStream(dest);
    https.get(url, {
      headers: { 'User-Agent': 'vibe-coding-tracker-npm-installer' }
    }, (response) => {
      if (response.statusCode === 302 || response.statusCode === 301) {
        // Follow redirect
        return downloadFile(response.headers.location, dest).then(resolve).catch(reject);
      }

      if (response.statusCode !== 200) {
        reject(new Error(`Failed to download: ${response.statusCode} ${response.statusMessage}`));
        return;
      }

      response.pipe(file);
      file.on('finish', () => {
        file.close();
        resolve();
      });
    }).on('error', (err) => {
      fs.unlink(dest, () => {});
      reject(err);
    });

    file.on('error', (err) => {
      fs.unlink(dest, () => {});
      reject(err);
    });
  });
}

// Extract binary from archive
async function extractBinary(archivePath, destDir, binaryName) {
  const ext = path.extname(archivePath);
  const binPath = path.join(destDir, binaryName);

  if (ext === '.zip') {
    // For Windows .zip files
    const AdmZip = require('adm-zip');
    const zip = new AdmZip(archivePath);
    zip.extractEntryTo(binaryName, destDir, false, true);
  } else if (ext === '.gz') {
    // For .tar.gz files
    await tar.extract({
      file: archivePath,
      cwd: destDir,
    });
  } else {
    throw new Error(`Unsupported archive format: ${ext}`);
  }

  // Make binary executable (Unix-like systems)
  if (process.platform !== 'win32') {
    fs.chmodSync(binPath, 0o755);
  }

  return binPath;
}

async function install() {
  try {
    const info = getPlatformInfo();
    console.log(`Installing vibe-coding-tracker v${VERSION} for ${info.friendlyId}...`);

    const binDir = path.join(__dirname, 'bin');
    if (!fs.existsSync(binDir)) {
      fs.mkdirSync(binDir, { recursive: true });
    }

    // Construct download URL
    const archiveExt = info.platform === 'win32' ? 'zip' : 'tar.gz';
    const archiveName = `${BINARY_NAME}-v${VERSION}-${info.friendlyId}.${archiveExt}`;
    const downloadUrl = `https://github.com/Mai0313/VibeCodingTracker/releases/download/v${VERSION}/${archiveName}`;

    const archivePath = path.join(binDir, archiveName);

    // Download archive
    await downloadFile(downloadUrl, archivePath);
    console.log('Download complete!');

    // Extract binary
    console.log('Extracting binary...');
    await extractBinary(archivePath, binDir, info.binaryName);

    // Clean up archive
    fs.unlinkSync(archivePath);

    console.log(`✓ Successfully installed ${BINARY_NAME} to ${binDir}`);
    console.log(`\nYou can now run: vct --help`);
  } catch (error) {
    console.error('Installation failed:', error.message);
    console.error('\nPlease install manually from:');
    console.error(`https://github.com/Mai0313/VibeCodingTracker/releases/tag/v${VERSION}`);
    process.exit(1);
  }
}

// Only run install if this script is executed directly
if (require.main === module) {
  install();
}

module.exports = { install };

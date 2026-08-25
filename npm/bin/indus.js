#!/usr/bin/env node

"use strict";

const crypto = require("node:crypto");
const fs = require("node:fs");
const https = require("node:https");
const os = require("node:os");
const path = require("node:path");
const { spawnSync } = require("node:child_process");

const VERSION = "0.1.0";
const REPOSITORY = "mciair/indus";
const MAX_REDIRECTS = 5;

function targetFor(platform, architecture) {
  const targets = {
    "darwin-arm64": "aarch64-apple-darwin",
    "darwin-x64": "x86_64-apple-darwin",
    "linux-arm64": "aarch64-unknown-linux-gnu",
    "linux-x64": "x86_64-unknown-linux-gnu",
    "win32-arm64": "aarch64-pc-windows-msvc",
    "win32-x64": "x86_64-pc-windows-msvc"
  };
  return targets[`${platform}-${architecture}`];
}

function cacheRoot() {
  if (process.env.INDUS_CACHE_DIR) {
    return process.env.INDUS_CACHE_DIR;
  }
  if (process.platform === "win32") {
    return path.join(process.env.LOCALAPPDATA || os.homedir(), "Indus", "Cache");
  }
  return path.join(process.env.XDG_CACHE_HOME || path.join(os.homedir(), ".cache"), "indus");
}

function download(url, destination, redirects = 0) {
  return new Promise((resolve, reject) => {
    const request = https.get(
      url,
      {
        headers: {
          Accept: "application/octet-stream",
          "User-Agent": `indus-cli/${VERSION}`
        }
      },
      (response) => {
        if (
          response.statusCode >= 300 &&
          response.statusCode < 400 &&
          response.headers.location
        ) {
          response.resume();
          if (redirects >= MAX_REDIRECTS) {
            reject(new Error(`Too many redirects while downloading ${url}`));
            return;
          }
          download(new URL(response.headers.location, url), destination, redirects + 1)
            .then(resolve, reject);
          return;
        }
        if (response.statusCode !== 200) {
          response.resume();
          reject(new Error(`Download failed with HTTP ${response.statusCode}: ${url}`));
          return;
        }
        const output = fs.createWriteStream(destination, { mode: 0o755 });
        response.pipe(output);
        output.on("finish", () => output.close(resolve));
        output.on("error", reject);
      }
    );
    request.on("error", reject);
  });
}

function downloadText(url, redirects = 0) {
  return new Promise((resolve, reject) => {
    const request = https.get(
      url,
      { headers: { "User-Agent": `indus-cli/${VERSION}` } },
      (response) => {
        if (
          response.statusCode >= 300 &&
          response.statusCode < 400 &&
          response.headers.location
        ) {
          response.resume();
          if (redirects >= MAX_REDIRECTS) {
            reject(new Error(`Too many redirects while downloading ${url}`));
            return;
          }
          downloadText(new URL(response.headers.location, url), redirects + 1)
            .then(resolve, reject);
          return;
        }
        if (response.statusCode !== 200) {
          response.resume();
          reject(new Error(`Download failed with HTTP ${response.statusCode}: ${url}`));
          return;
        }
        response.setEncoding("utf8");
        let body = "";
        response.on("data", (chunk) => {
          body += chunk;
        });
        response.on("end", () => resolve(body));
      }
    );
    request.on("error", reject);
  });
}

function sha256(file) {
  const hash = crypto.createHash("sha256");
  hash.update(fs.readFileSync(file));
  return hash.digest("hex");
}

async function installedBinary() {
  if (process.env.INDUS_BINARY_PATH) {
    return process.env.INDUS_BINARY_PATH;
  }

  const target = targetFor(process.platform, process.arch);
  if (!target) {
    throw new Error(`Indus does not yet publish a binary for ${process.platform}/${process.arch}`);
  }
  const extension = process.platform === "win32" ? ".exe" : "";
  const asset = `indus-${target}${extension}`;
  const directory = path.join(cacheRoot(), VERSION, target);
  const binary = path.join(directory, asset);
  if (fs.existsSync(binary)) {
    return binary;
  }

  fs.mkdirSync(directory, { recursive: true });
  const temporary = `${binary}.${process.pid}.download`;
  const base = process.env.INDUS_DOWNLOAD_BASE_URL ||
    `https://github.com/${REPOSITORY}/releases/download/v${VERSION}`;
  process.stderr.write(`Downloading Indus v${VERSION} for ${target}…\n`);
  try {
    await download(`${base}/${asset}`, temporary);
    const checksum = (await downloadText(`${base}/${asset}.sha256`)).trim().split(/\s+/)[0];
    const actual = sha256(temporary);
    if (!/^[a-fA-F0-9]{64}$/.test(checksum) || actual !== checksum.toLowerCase()) {
      throw new Error(`Checksum verification failed for ${asset}`);
    }
    fs.chmodSync(temporary, 0o755);
    fs.renameSync(temporary, binary);
  } finally {
    fs.rmSync(temporary, { force: true });
  }
  return binary;
}

async function main() {
  const binary = await installedBinary();
  const result = spawnSync(binary, process.argv.slice(2), { stdio: "inherit" });
  if (result.error) {
    throw result.error;
  }
  if (result.signal) {
    process.kill(process.pid, result.signal);
    return;
  }
  process.exitCode = result.status === null ? 1 : result.status;
}

main().catch((error) => {
  process.stderr.write(`indus: ${error.message}\n`);
  process.exitCode = 1;
});

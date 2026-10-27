#!/usr/bin/env node
// Downloads the pinned llama.cpp macOS arm64 build and puts `llama-server`
// (as `llama-server-darwin-arm64`) and its dylibs in resources/bin, which the
// app bundles and, in debug builds, runs from directly. Build-time only.
//
//   node scripts/download-llama-server.js [--force]
//
// LLAMA_CPP_VERSION overrides the pinned tag; GITHUB_TOKEN raises the API
// rate limit. A matching install (version stamp) is left alone.
const { execFileSync } = require("child_process");
const fs = require("fs");
const os = require("os");
const path = require("path");

const REPO = "ggml-org/llama.cpp";
const TAG = process.env.LLAMA_CPP_VERSION || "b9763";
const ASSET_PATTERN = /^llama-.*-bin-macos-arm64\.tar\.gz$/;
const OUTPUT_NAME = "llama-server-darwin-arm64";
const BIN_DIR = path.join(__dirname, "..", "resources", "bin");
const STAMP = path.join(BIN_DIR, "llama-server.version");

const force = process.argv.includes("--force");

function installedVersion() {
  try {
    return fs.readFileSync(STAMP, "utf8").trim();
  } catch {
    return null;
  }
}

async function fetchRelease() {
  const headers = { Accept: "application/vnd.github+json", "User-Agent": "retone-build" };
  if (process.env.GITHUB_TOKEN) headers.Authorization = `Bearer ${process.env.GITHUB_TOKEN}`;
  const response = await fetch(`https://api.github.com/repos/${REPO}/releases/tags/${TAG}`, {
    headers,
  });
  if (!response.ok) throw new Error(`GitHub API returned ${response.status} for ${TAG}`);
  return response.json();
}

async function download(url, destination) {
  const response = await fetch(url, { headers: { "User-Agent": "retone-build" } });
  if (!response.ok) throw new Error(`Download failed with status ${response.status}`);
  fs.writeFileSync(destination, Buffer.from(await response.arrayBuffer()));
}

function findFile(dir, name) {
  for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
    const full = path.join(dir, entry.name);
    if (entry.isDirectory()) {
      const found = findFile(full, name);
      if (found) return found;
    } else if (entry.name === name) {
      return full;
    }
  }
  return null;
}

async function main() {
  if (process.platform !== "darwin" || process.arch !== "arm64") {
    console.log(`[llama-server] Skipped: ReTone bundles llama-server for darwin-arm64 only.`);
    return;
  }
  const outputPath = path.join(BIN_DIR, OUTPUT_NAME);
  if (!force && fs.existsSync(outputPath) && installedVersion() === TAG) {
    console.log(`[llama-server] ${TAG} already in resources/bin (use --force to re-download)`);
    return;
  }

  console.log(`[llama-server] Fetching llama.cpp ${TAG} for darwin-arm64...`);
  const release = await fetchRelease();
  const asset = release.assets?.find((a) => ASSET_PATTERN.test(a.name));
  if (!asset) throw new Error(`No asset matching ${ASSET_PATTERN} in ${TAG}`);

  const work = fs.mkdtempSync(path.join(os.tmpdir(), "retone-llama-"));
  try {
    const archive = path.join(work, asset.name);
    console.log(`[llama-server] Downloading ${asset.browser_download_url}`);
    await download(asset.browser_download_url, archive);
    const extracted = path.join(work, "x");
    fs.mkdirSync(extracted);
    execFileSync("tar", ["-xzf", archive, "-C", extracted]);

    const binary = findFile(extracted, "llama-server");
    if (!binary) throw new Error("llama-server not found in the archive");

    fs.mkdirSync(BIN_DIR, { recursive: true });
    fs.copyFileSync(binary, outputPath);
    fs.chmodSync(outputPath, 0o755);
    const libDir = path.dirname(binary);
    const libs = fs.readdirSync(libDir).filter((name) => name.endsWith(".dylib"));
    for (const lib of libs) {
      // Follows symlinks: every name the loader may ask for becomes a file.
      fs.copyFileSync(path.join(libDir, lib), path.join(BIN_DIR, lib));
    }
    fs.writeFileSync(STAMP, `${TAG}\n`);
    console.log(`[llama-server] Installed ${OUTPUT_NAME} and ${libs.length} libraries (${TAG})`);
  } finally {
    fs.rmSync(work, { recursive: true, force: true });
  }
}

main().catch((error) => {
  console.error(`[llama-server] ${error.message}`);
  process.exitCode = 1;
});

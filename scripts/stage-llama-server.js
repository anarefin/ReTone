#!/usr/bin/env node
// Stages the llama-server that ships inside ReTone.app: copies the binary and
// only the dylibs it links (resolved with otool through @rpath, which is
// @loader_path) from resources/bin into resources/llama-bundle, then signs
// them there: every dylib ad-hoc, llama-server ad-hoc with the hardened
// runtime and resources/mac/llama-server.entitlements.plist. tauri.conf.json
// bundles resources/llama-bundle/* as Contents/Resources/bin/. Build-time only.
//
//   node scripts/stage-llama-server.js
//
// APPLE_SIGNING_IDENTITY overrides the ad-hoc identity ("-"), matching what
// the Tauri bundler signs the app with.
const { execFileSync } = require("child_process");
const fs = require("fs");
const path = require("path");

const ROOT = path.join(__dirname, "..");
const SOURCE = path.join(ROOT, "resources", "bin");
const STAGE = path.join(ROOT, "resources", "llama-bundle");
const BINARY = "llama-server-darwin-arm64";
const ENTITLEMENTS = path.join(ROOT, "resources", "mac", "llama-server.entitlements.plist");
const IDENTITY = process.env.APPLE_SIGNING_IDENTITY || "-";
const IDENTIFIER = "com.arefin.retone.llama-server";

function run(cmd, args) {
  return execFileSync(cmd, args, { encoding: "utf8", stdio: ["ignore", "pipe", "pipe"] });
}

// Every @rpath dependency, transitively, starting from the binary.
function closure(start) {
  const seen = new Set();
  const todo = [start];
  while (todo.length) {
    const name = todo.pop();
    if (seen.has(name)) continue;
    const file = path.join(SOURCE, name);
    if (!fs.existsSync(file)) throw new Error(`${name} is linked but missing from resources/bin`);
    seen.add(name);
    for (const line of run("otool", ["-L", file]).split("\n").slice(1)) {
      const dep = line.trim().split(" ")[0];
      if (dep.startsWith("@rpath/")) todo.push(dep.slice("@rpath/".length));
    }
  }
  return [...seen].sort();
}

function sign(file, extra = []) {
  run("codesign", ["--force", "--sign", IDENTITY, "--timestamp=none", ...extra, file]);
  run("codesign", ["--verify", "--strict", file]);
}

function main() {
  if (process.platform !== "darwin") {
    console.log("[stage-llama-server] Skipped: macOS only.");
    return;
  }
  if (!fs.existsSync(path.join(SOURCE, BINARY))) {
    throw new Error(`resources/bin/${BINARY} is missing: run npm run download:llama-server`);
  }
  const files = closure(BINARY);
  fs.rmSync(STAGE, { recursive: true, force: true });
  fs.mkdirSync(STAGE, { recursive: true });
  for (const name of files) {
    // Dereferences symlinks: the bundle holds plain files.
    fs.copyFileSync(path.join(SOURCE, name), path.join(STAGE, name));
    fs.chmodSync(path.join(STAGE, name), 0o755);
  }
  for (const name of files.filter((n) => n !== BINARY)) {
    sign(path.join(STAGE, name));
  }
  sign(path.join(STAGE, BINARY), [
    "--options",
    "runtime",
    "--entitlements",
    ENTITLEMENTS,
    "--identifier",
    IDENTIFIER,
  ]);
  const bytes = files.reduce((sum, n) => sum + fs.statSync(path.join(STAGE, n)).size, 0);
  console.log(
    `[stage-llama-server] Staged and signed ${files.length} files (${(bytes / 1e6).toFixed(1)} MB, identity "${IDENTITY}") in resources/llama-bundle`
  );
}

try {
  main();
} catch (error) {
  console.error(`[stage-llama-server] ${error.message}`);
  process.exit(1);
}

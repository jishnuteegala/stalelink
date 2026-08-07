#!/usr/bin/env node

const { execFileSync } = require("node:child_process");
const { existsSync, mkdirSync } = require("node:fs");
const { homedir, tmpdir } = require("node:os");
const { join } = require("node:path");

const schemaCommit = "7e3dc73b83c92e0e99aa50b599f24c64c79574e2";
const schemaUrl = `https://raw.githubusercontent.com/ScoopInstaller/Scoop/${schemaCommit}/schema.json`;
const manifest = process.argv[2];

if (!manifest) {
  throw new Error("usage: validate-scoop-manifest.js <manifest.json>");
}

const cacheDir = join(process.env.XDG_CACHE_HOME || join(homedir() || tmpdir(), ".cache"), "stalelink");
const schema = join(cacheDir, `scoop-schema-${schemaCommit}.json`);
mkdirSync(cacheDir, { recursive: true });
if (!existsSync(schema)) {
  execFileSync("curl", ["--proto", "=https", "--tlsv1.2", "-LsSf", "-o", schema, schemaUrl], { stdio: "inherit" });
}

const args = ["--yes", "--package=ajv-cli@5.0.0", "ajv", "validate", "--spec=draft7", "--strict=false", "--validate-formats=false", "-s", schema, "-d", manifest];
if (process.platform === "win32") {
  execFileSync("npx", args, { shell: true, stdio: "inherit" });
} else {
  execFileSync("npx", args, { stdio: "inherit" });
}

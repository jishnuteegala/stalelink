const { spawnSync } = require("node:child_process");

const result = spawnSync("dist", ["plan", "--output-format=json"], {
  encoding: "utf8",
});
if (result.status !== 0) {
  process.stderr.write(result.stderr);
  process.exit(result.status ?? 1);
}

const plan = JSON.parse(result.stdout);
const serialized = JSON.stringify(plan);
if (!serialized.includes("stalelink-npm-package.tar.gz")) {
  throw new Error("cargo-dist plan does not include the generated npm installer artifact");
}

const targets = [
  "aarch64-apple-darwin",
  "x86_64-apple-darwin",
  "aarch64-unknown-linux-gnu",
  "x86_64-unknown-linux-gnu",
  "aarch64-pc-windows-msvc",
  "x86_64-pc-windows-msvc",
];
for (const target of targets) {
  if (!serialized.includes(target)) {
    throw new Error(`cargo-dist plan is missing ${target}`);
  }
}

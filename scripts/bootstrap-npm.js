const { execFileSync, spawnSync } = require("node:child_process");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");

const [tag, source = "draft"] = process.argv.slice(2);
if (!/^v\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?$/.test(tag || "")) {
  throw new Error("usage: node scripts/bootstrap-npm.js vX.Y.Z [draft|build]");
}
if (!["draft", "build"].includes(source)) throw new Error("source must be draft or build");

const version = tag.slice(1);
const directory = fs.mkdtempSync(path.join(os.tmpdir(), "stalelink-npm-bootstrap-"));
try {
  if (source === "draft") {
    execFileSync("gh", ["release", "download", tag, "--repo", "jishnuteegala/stalelink", "--pattern", "stalelink-npm-package.tar.gz", "--dir", directory], { stdio: "inherit" });
  } else {
    const root = execFileSync("git", ["rev-parse", "--show-toplevel"], { encoding: "utf8" }).trim();
    const checkedOutTag = execFileSync("git", ["describe", "--exact-match", "--tags", "HEAD"], { encoding: "utf8" }).trim();
    if (checkedOutTag !== tag) throw new Error(`local build requires checkout at ${tag}, found ${checkedOutTag}`);
    execFileSync("dist", ["build", `--tag=${tag}`, "--artifacts=global"], { cwd: root, stdio: "inherit" });
    fs.copyFileSync(path.join(root, "target", "distrib", "stalelink-npm-package.tar.gz"), path.join(directory, "stalelink-npm-package.tar.gz"));
  }
  const archive = path.join(directory, "stalelink-npm-package.tar.gz");
  execFileSync("tar", ["-xzf", archive, "-C", directory], { stdio: "inherit" });
  const packageDir = path.join(directory, "package");
  const manifest = JSON.parse(fs.readFileSync(path.join(packageDir, "package.json"), "utf8"));
  if (manifest.name !== "stalelink" || manifest.version !== version) {
    throw new Error(`package verification failed: expected stalelink@${version}, found ${manifest.name}@${manifest.version}`);
  }
  console.log(`Verified ${manifest.name}@${manifest.version}. Publishing with your current npm login...`);
  const result = spawnSync("npm", ["publish"], { cwd: packageDir, stdio: "inherit" });
  if (result.status !== 0) process.exitCode = result.status || 1;
  else console.log("Configure npm trusted publishing: package stalelink, repository jishnuteegala/stalelink, workflow .github/workflows/release.yml. Rerun the release workflow for this tag.");
} finally {
  fs.rmSync(directory, { recursive: true, force: true });
}

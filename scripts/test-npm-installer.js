const { spawnSync } = require("node:child_process");
const http = require("node:http");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");

const root = path.resolve(__dirname, "..");
const temp = fs.mkdtempSync(path.join(os.tmpdir(), "stalelink-npm-smoke-"));
const packageDir = path.join(temp, "package");

function tarPath(value) {
  if (process.platform !== "win32") return value;
  return value.replace(/^([A-Z]):\\/i, (_, drive) => `/${drive.toLowerCase()}/`).replaceAll("\\", "/");
}

function run(command, args, options = {}) {
  const result = spawnSync(command, args, {
    encoding: "utf8",
    ...options,
  });
  if (result.status !== 0) {
    if (result.stderr) process.stderr.write(result.stderr);
    if (result.stdout) process.stderr.write(result.stdout);
    throw new Error(`${command} exited ${result.status ?? result.error?.message ?? 1}`);
  }
}

function runAsync(command, args, options = {}) {
  return new Promise((resolve, reject) => {
    const child = require("node:child_process").spawn(command, args, {
      stdio: "pipe",
      ...options,
    });
    let stderr = "";
    child.stderr.on("data", (chunk) => (stderr += chunk));
    child.on("error", reject);
    child.on("close", (status) => {
      if (status === 0) resolve();
      else reject(new Error(`${command} exited ${status ?? 1}: ${stderr}`));
    });
  });
}

function nativeTarget() {
  const arch = process.arch === "x64" ? "x86_64" : "aarch64";
  const platform = {
    darwin: "apple-darwin",
    linux: "unknown-linux-gnu",
    win32: "pc-windows-msvc",
  }[process.platform];
  if (!platform) throw new Error(`unsupported test platform ${process.platform}`);
  return `${arch}-${platform}`;
}

function makeFakeArtifact(artifact) {
  const binary = process.platform === "win32" ? "stalelink.exe" : "stalelink";
  const contents = path.join(temp, "artifact");
  fs.mkdirSync(contents);
  fs.copyFileSync(process.execPath, path.join(contents, binary));
  if (process.platform === "win32") {
    run("powershell.exe", [
      "-NoProfile",
      "-NonInteractive",
      "-Command",
      `Compress-Archive -Path '${contents}\\*' -DestinationPath '${artifact}' -Force`,
    ]);
  } else {
    run("tar", ["--force-local", "-cJf", tarPath(artifact), "-C", tarPath(temp), "artifact"]);
  }
}

async function main() {
  run("dist", ["build", "--artifacts=global", "--output-format=json"], { cwd: root });
  const installer = path.join(root, "target", "distrib", "stalelink-npm-package.tar.gz");
  run("tar", ["--force-local", "-xzf", tarPath(installer), "-C", tarPath(temp)]);
  run(process.execPath, [process.env.npm_execpath, "pack", "--dry-run"], {
    cwd: packageDir,
  });

  // The generated package needs no network dependency for this controlled smoke.
  const libc = path.join(packageDir, "node_modules", "detect-libc");
  fs.mkdirSync(libc, { recursive: true });
  fs.writeFileSync(
    path.join(libc, "index.js"),
    "exports.familySync = () => 'glibc'; exports.isNonGlibcLinuxSync = () => false; exports.versionSync = () => '2.31';\n",
  );

  const manifestPath = path.join(packageDir, "package.json");
  const manifest = JSON.parse(fs.readFileSync(manifestPath, "utf8"));
  const target = nativeTarget();
  const platform = manifest.supportedPlatforms[target];
  if (!platform) throw new Error(`generated package does not support ${target}`);

  const artifact = path.join(temp, platform.artifactName);
  makeFakeArtifact(artifact);
  const server = http.createServer((request, response) => {
    if (request.url === `/${platform.artifactName}`) {
      response.writeHead(200, { "content-type": "application/octet-stream" });
      fs.createReadStream(artifact).pipe(response);
      return;
    }
    response.writeHead(404).end();
  });
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));

  try {
    const { port } = server.address();
    manifest.artifactDownloadUrls = [`http://127.0.0.1:${port}`];
    fs.writeFileSync(manifestPath, `${JSON.stringify(manifest, null, 2)}\n`);

    // getPackage verifies the generated resolver chooses this runner's target.
    const { getPackage } = require(path.join(packageDir, "binary.js"));
    if (getPackage().platform.artifactName !== platform.artifactName) {
      throw new Error(`resolver did not select ${target}`);
    }

    // run-stalelink triggers install.js's real download/extract/launcher path.
    await runAsync(process.execPath, [
      path.join(packageDir, "run-stalelink.js"),
      "--version",
    ]);
  } finally {
    await new Promise((resolve) => server.close(resolve));
  }
}

main()
  .catch((error) => {
    console.error(error.stack);
    process.exitCode = 1;
  })
  .finally(() => fs.rmSync(temp, { recursive: true, force: true }));

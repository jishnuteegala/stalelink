import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { chmodSync, mkdtempSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { spawnSync } from "node:child_process";
import { delimiter, join, resolve } from "node:path";
import { tmpdir } from "node:os";
import test from "node:test";

const root = resolve(import.meta.dirname, "..", "..");
const powershell = process.platform === "win32" ? "powershell.exe" : "pwsh";

test("Chocolatey package uses public Windows archives and checksums", () => {
  const work = mkdtempSync(join(tmpdir(), "stalelink-choco-"));
  const dist = join(work, "target", "distrib");
  const bin = join(work, "bin");
  const temp = join(work, "temp");
  mkdirSync(dist, { recursive: true });
  mkdirSync(bin);
  mkdirSync(temp);
  const x64 = createHash("sha256").update("x64 archive").digest("hex");
  const arm64 = createHash("sha256").update("arm64 archive").digest("hex");
  writeFileSync(join(dist, "sha256.sum"), `${x64}  stalelink-x86_64-pc-windows-msvc.zip\n${arm64}  stalelink-aarch64-pc-windows-msvc.zip\n`);
  const choco = process.platform === "win32" ? join(bin, "choco.cmd") : join(bin, "choco");
  writeFileSync(choco, process.platform === "win32" ? "@echo off\r\ntype nul > %4\\stalelink.1.2.3.nupkg\r\n" : "#!/bin/sh\ntouch \"$4/stalelink.1.2.3.nupkg\"\n");
  if (process.platform !== "win32") chmodSync(choco, 0o755);

  const result = spawnSync(powershell, ["-NoProfile", "-File", join(root, "packaging", "scripts", "package-chocolatey.ps1"), "-Version", "1.2.3"], {
    encoding: "utf8",
    env: { ...process.env, Path: `${bin}${delimiter}${process.env.Path}`, PUBLISH_CHOCOLATEY_ROOT: work, PUBLISH_CHOCOLATEY_CLI: choco, TEMP: temp },
  });
  try {
    assert.equal(result.status, 0, result.stderr || result.stdout);
    const stage = join(temp, "stalelink-chocolatey-1.2.3");
    const nuspec = readFileSync(join(stage, "stalelink.nuspec"), "utf8");
    const install = readFileSync(join(stage, "tools", "chocolateyinstall.ps1"), "utf8");
    assert.match(nuspec, /<version>1\.2\.3<\/version>/);
    assert.match(install, /stalelink-x86_64-pc-windows-msvc\.zip/);
    assert.match(install, /stalelink-aarch64-pc-windows-msvc\.zip/);
    assert.match(install, new RegExp(x64));
    assert.match(install, new RegExp(arm64));
    assert.match(install, /RuntimeInformation.*OSArchitecture.*Arm64/);
  } finally {
    rmSync(work, { recursive: true, force: true });
  }
});

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

test("Chocolatey publisher verifies canonical files in an existing public package", () => {
  const work = mkdtempSync(join(tmpdir(), "stalelink-choco-publish-"));
  const dist = join(work, "target", "distrib");
  const temp = join(work, "temp");
  const localFiles = join(work, "local");
  const remoteFiles = join(work, "remote");
  const localPackage = join(dist, "stalelink.1.2.3.nupkg");
  const remotePackage = join(work, "remote.nupkg");
  mkdirSync(join(localFiles, "tools"), { recursive: true });
  mkdirSync(join(remoteFiles, "tools"), { recursive: true });
  mkdirSync(dist, { recursive: true });
  mkdirSync(temp);
  writeFileSync(join(localFiles, "stalelink.nuspec"), "canonical nuspec");
  writeFileSync(join(localFiles, "tools", "chocolateyinstall.ps1"), "canonical installer");
  writeFileSync(join(remoteFiles, "stalelink.nuspec"), "canonical nuspec");
  writeFileSync(join(remoteFiles, "tools", "chocolateyinstall.ps1"), "canonical installer");
  const wrapper = join(work, "publish.ps1");
  writeFileSync(wrapper, `
param([string]$Publisher, [string]$LocalFiles, [string]$RemoteFiles)
Add-Type -AssemblyName System.IO.Compression.FileSystem
Remove-Item $env:LOCAL_PACKAGE, $env:REMOTE_PACKAGE -Force -ErrorAction SilentlyContinue
[System.IO.Compression.ZipFile]::CreateFromDirectory($LocalFiles, $env:LOCAL_PACKAGE)
[System.IO.Compression.ZipFile]::CreateFromDirectory($RemoteFiles, $env:REMOTE_PACKAGE)
function Invoke-WebRequest {
  param([string]$Uri, [string]$OutFile, [switch]$UseBasicParsing)
  if ($Uri -like '*Packages()?*') { return [pscustomobject]@{ Content = '<entry>' } }
  if ($Uri -like '*/package/stalelink/1.2.3') { Copy-Item $env:REMOTE_PACKAGE $OutFile; return }
  throw "unexpected URI: $Uri"
}
& $Publisher -Version 1.2.3
`);
  const env = {
    ...process.env,
    CHOCOLATEY_API_KEY: "test-key",
    LOCAL_PACKAGE: localPackage,
    PUBLISH_CHOCOLATEY_ROOT: work,
    REMOTE_PACKAGE: remotePackage,
    TEMP: temp,
  };
  try {
    const matching = spawnSync(powershell, ["-NoProfile", "-File", wrapper, join(root, "packaging", "scripts", "publish-chocolatey.ps1"), localFiles, remoteFiles], { encoding: "utf8", env });
    assert.equal(matching.status, 0, matching.stderr || matching.stdout);
    assert.match(matching.stdout, /verified public Chocolatey stalelink 1\.2\.3/);

    writeFileSync(join(remoteFiles, "tools", "chocolateyinstall.ps1"), "conflicting installer");
    const conflicting = spawnSync(powershell, ["-NoProfile", "-File", wrapper, join(root, "packaging", "scripts", "publish-chocolatey.ps1"), localFiles, remoteFiles], { encoding: "utf8", env });
    assert.notEqual(conflicting.status, 0);
    assert.match(`${conflicting.stderr}\n${conflicting.stdout}`, /conflicts with the canonical package/);
  } finally {
    rmSync(work, { recursive: true, force: true });
  }
});

param(
    [Parameter(Mandatory = $true)]
    [ValidatePattern('^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(-[0-9A-Za-z.-]+)?$')]
    [string]$Version
)

$ErrorActionPreference = 'Stop'
$root = if ($env:PUBLISH_CHOCOLATEY_ROOT) { $env:PUBLISH_CHOCOLATEY_ROOT } else { Split-Path (Split-Path $PSScriptRoot -Parent) -Parent }
$dist = Join-Path $root 'target/distrib'
$checksums = Join-Path $dist 'sha256.sum'
if (-not (Test-Path $checksums)) { throw "missing $checksums" }

function Get-ReleaseChecksum([string]$Architecture) {
    $name = "stalelink-$Architecture-pc-windows-msvc.zip"
    $line = Select-String -Path $checksums -Pattern "^[a-fA-F0-9]{64} [ *]$([regex]::Escape($name))`$"
    if (@($line).Count -ne 1) { throw "expected one checksum for $name" }
    return $line.Line.Substring(0, 64).ToLowerInvariant()
}

$x64 = Get-ReleaseChecksum 'x86_64'
$arm64 = Get-ReleaseChecksum 'aarch64'
$stage = Join-Path $env:TEMP "stalelink-chocolatey-$Version"
Remove-Item $stage -Recurse -Force -ErrorAction SilentlyContinue
New-Item (Join-Path $stage 'tools') -ItemType Directory -Force | Out-Null

$nuspec = @"
<?xml version="1.0" encoding="utf-8"?>
<package xmlns="http://schemas.microsoft.com/packaging/2015/06/nuspec.xsd">
  <metadata>
    <id>stalelink</id>
    <version>$Version</version>
    <title>stalelink</title>
    <authors>Jishnu Teegala</authors>
    <owners>Jishnu Teegala</owners>
    <licenseUrl>https://github.com/jishnuteegala/stalelink/blob/main/LICENSE</licenseUrl>
    <projectUrl>https://github.com/jishnuteegala/stalelink</projectUrl>
    <packageSourceUrl>https://github.com/jishnuteegala/stalelink</packageSourceUrl>
    <requireLicenseAcceptance>false</requireLicenseAcceptance>
    <description>Find dead and outdated links in local documents.</description>
    <summary>Scan local documents for dead and outdated links</summary>
    <releaseNotes>https://github.com/jishnuteegala/stalelink/releases/tag/v$Version</releaseNotes>
    <tags>link checker documents cli</tags>
  </metadata>
</package>
"@
Set-Content -Path (Join-Path $stage 'stalelink.nuspec') -Value $nuspec -Encoding UTF8

$install = @"
`$ErrorActionPreference = 'Stop'
`$tools = Split-Path -Parent `$MyInvocation.MyCommand.Definition
`$packageArgs = @{
  packageName    = 'stalelink'
  unzipLocation  = `$tools
  url64bit       = 'https://github.com/jishnuteegala/stalelink/releases/download/v$Version/stalelink-x86_64-pc-windows-msvc.zip'
  checksum64     = '$x64'
  checksumType64 = 'sha256'
}

if ([System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture -eq [System.Runtime.InteropServices.Architecture]::Arm64) {
  `$packageArgs.url64bit = 'https://github.com/jishnuteegala/stalelink/releases/download/v$Version/stalelink-aarch64-pc-windows-msvc.zip'
  `$packageArgs.checksum64 = '$arm64'
}
Install-ChocolateyZipPackage @packageArgs
"@
Set-Content -Path (Join-Path $stage 'tools/chocolateyinstall.ps1') -Value $install -Encoding UTF8

$choco = if ($env:PUBLISH_CHOCOLATEY_CLI) { $env:PUBLISH_CHOCOLATEY_CLI } else { 'choco' }
& $choco pack (Join-Path $stage 'stalelink.nuspec') --outputdirectory $dist --limit-output
if ($LASTEXITCODE -ne 0) { throw 'choco pack failed' }
$package = Join-Path $dist "stalelink.$Version.nupkg"
if (-not (Test-Path $package)) { throw "missing generated package $package" }
Write-Output "generated $package"

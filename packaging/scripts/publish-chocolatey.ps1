param(
    [Parameter(Mandatory = $true)]
    [ValidatePattern('^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(-[0-9A-Za-z.-]+)?$')]
    [string]$Version
)

$ErrorActionPreference = 'Stop'
if (-not $env:CHOCOLATEY_API_KEY) { throw 'CHOCOLATEY_API_KEY is required' }
$root = if ($env:PUBLISH_CHOCOLATEY_ROOT) { $env:PUBLISH_CHOCOLATEY_ROOT } else { Split-Path (Split-Path $PSScriptRoot -Parent) -Parent }
$dist = Join-Path $root 'target/distrib'
$package = Join-Path $dist "stalelink.$Version.nupkg"
if (-not (Test-Path $package)) {
    & "$PSScriptRoot/package-chocolatey.ps1" -Version $Version
    if ($LASTEXITCODE -ne 0) { throw 'Chocolatey package generation failed' }
}
if (-not (Test-Path $package)) { throw "missing generated package $package" }

$feed = 'https://community.chocolatey.org/api/v2'
$query = "$feed/Packages()?`$filter=Id%20eq%20%27stalelink%27%20and%20Version%20eq%20%27$Version%27"
$response = Invoke-WebRequest -Uri $query -UseBasicParsing
if ($response.Content -match '<entry>') {
    Write-Output "verified public Chocolatey stalelink $Version"
    exit 0
}

$output = & choco push $package --source https://push.chocolatey.org/ --api-key $env:CHOCOLATEY_API_KEY 2>&1
if ($LASTEXITCODE -ne 0) { throw "Chocolatey push failed: $($output -join "`n")" }
Write-Output "Chocolatey accepted stalelink $Version; public availability awaits moderation"

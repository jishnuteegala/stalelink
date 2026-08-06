param(
  [int]$Iterations = 9,
  [int]$ColdTrials = 10,
  [int]$WarmupPasses = 2,
  [int]$TimedPasses = 7
)
$ErrorActionPreference = 'Stop'
$root = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$corpus = Join-Path $root 'bench/corpus'
$validation = Join-Path $root 'bench/validation'
$results = Join-Path $root 'bench/results'
New-Item -ItemType Directory -Force $results | Out-Null
Add-Type @'
using System;
using System.Runtime.InteropServices;
public static class PeakWorkingSet {
  [StructLayout(LayoutKind.Sequential)] public struct Counters {
    public uint cb, PageFaultCount; public UIntPtr PeakWorkingSetSize, WorkingSetSize, QuotaPeakPagedPoolUsage, QuotaPagedPoolUsage, QuotaPeakNonPagedPoolUsage, QuotaNonPagedPoolUsage, PagefileUsage, PeakPagefileUsage, PrivateUsage;
  }
  [DllImport("psapi.dll", SetLastError=true)] static extern bool GetProcessMemoryInfo(IntPtr process, out Counters counters, uint size);
  public static ulong Read(IntPtr handle) { Counters c; return GetProcessMemoryInfo(handle, out c, (uint)Marshal.SizeOf(typeof(Counters))) ? c.PeakWorkingSetSize.ToUInt64() : 0; }
}
'@

function Get-RecordDigest($receipt) {
  $records = @($receipt.records | ForEach-Object {
    "$($_.doc)|$($_.url)|$($_.location.type)|$($_.location.line)|$($_.location.column)|$($_.location.paragraph)|$($_.location.sheet)|$($_.location.cell)|$($_.location.slide)|$($_.location.page)|$($_.location.annotation)|$($_.span -join ':')"
  } | Sort-Object) -join "`n"
  $bytes = [Text.Encoding]::UTF8.GetBytes($records)
  $sha = [Security.Cryptography.SHA256]::Create()
  (($sha.ComputeHash($bytes) | ForEach-Object { $_.ToString('x2') }) -join '')
}

function Get-Median([double[]]$values) {
  $ordered = @($values | Sort-Object)
  $middle = [int]($ordered.Count / 2)
  if (($ordered.Count % 2) -eq 1) { return $ordered[$middle] }
  return ($ordered[$middle - 1] + $ordered[$middle]) / 2
}

function Invoke-Harness($name, $program, [string[]]$arguments) {
  $info = [Diagnostics.ProcessStartInfo]::new($program)
  $info.UseShellExecute = $false
  $info.RedirectStandardOutput = $true
  $info.RedirectStandardError = $true
  $info.Arguments = (($arguments | ForEach-Object { '"' + ($_ -replace '"', '\"') + '"' }) -join ' ')
  $process = [Diagnostics.Process]::new()
  $process.StartInfo = $info
  $watch = [Diagnostics.Stopwatch]::StartNew()
  [void]$process.Start()
  $process.Handle | Out-Null # Keep the native process handle alive for post-exit peak accounting.
  $stdout = $process.StandardOutput.ReadToEndAsync()
  $stderr = $process.StandardError.ReadToEndAsync()
  $process.WaitForExit()
  $watch.Stop()
  $output = $stdout.GetAwaiter().GetResult()
  $errors = $stderr.GetAwaiter().GetResult()
  $exitCode = $process.ExitCode
  $process.Refresh() # PeakWorkingSetSize is a kernel-maintained process-lifetime maximum.
  if ($exitCode -ne 0) { throw "$name exited ${exitCode}: $errors" }
  try { $receipt = $output | ConvertFrom-Json } catch { throw "$name emitted invalid JSON: $output`n$errors" }
  if ($null -eq $receipt.links -or $null -eq $receipt.documents -or $null -eq $receipt.records) { throw "$name emitted an incomplete receipt" }
  $peak = [PeakWorkingSet]::Read($process.Handle)
  if ($peak -le 0) {
    $native = [Diagnostics.Process]::GetProcessById($process.Id)
    $native.Handle | Out-Null
    $native.Refresh()
    $peak = [PeakWorkingSet]::Read($native.Handle)
  }
  if ($peak -le 0) { throw "$name did not expose PeakWorkingSetSize after exit; refusing to label a sampled value as peak" }
  [pscustomobject]@{ Receipt=$receipt; ElapsedMs=$watch.Elapsed.TotalMilliseconds; PeakWorkingSetBytes=$peak }
}

function Assert-Equivalent($rust, $go, $context) {
  $rustDigest = Get-RecordDigest $rust.Receipt
  $goDigest = Get-RecordDigest $go.Receipt
  if ($rust.Receipt.documents -ne $go.Receipt.documents -or $rust.Receipt.links -ne $go.Receipt.links -or $rustDigest -ne $goDigest) {
    throw "$context differs: Rust docs/links/digest=$($rust.Receipt.documents)/$($rust.Receipt.links)/$rustDigest; Go=$($go.Receipt.documents)/$($go.Receipt.links)/$goDigest"
  }
  $rustDigest
}

function Get-ExpectedDocumentCount([string]$directory, [string]$format = '') {
  @(Get-ChildItem -LiteralPath $directory -File | Where-Object { $format -eq '' -or $_.Extension -eq ".$format" }).Count
}

function Assert-DocumentCount($run, [int]$expected, $context) {
  if ($run.Receipt.documents -ne $expected) { throw "$context counted $($run.Receipt.documents) documents; expected $expected matching input files" }
}

Push-Location $root
try {
  cargo run --quiet --release --manifest-path bench/rust-harness/Cargo.toml -- generate $corpus

  cargo run --quiet --release --manifest-path bench/rust-harness/Cargo.toml -- generate-validation $validation
  cargo fmt --manifest-path bench/rust-harness/Cargo.toml -- --check
  cargo clippy --manifest-path bench/rust-harness/Cargo.toml --all-targets -- -D warnings
  cargo build --quiet --release --manifest-path bench/rust-harness/Cargo.toml
  go -C bench/go-port build -o stalelink-go.exe .
  $rust = Join-Path $root 'bench/rust-harness/target/release/stalelink-bench-harness.exe'
  $go = Join-Path $root 'bench/go-port/stalelink-go.exe'

  $validationRust = Invoke-Harness Rust $rust @('extract', $validation)
  $validationGo = Invoke-Harness Go $go @('extract', $validation)
  Assert-DocumentCount $validationRust (Get-ExpectedDocumentCount $validation) 'validation Rust extraction'
  Assert-DocumentCount $validationGo (Get-ExpectedDocumentCount $validation) 'validation Go extraction'
  Assert-Equivalent $validationRust $validationGo 'validation extraction' | Out-Null

  $baselineRust = Invoke-Harness Rust $rust @('extract', $corpus)
  $baselineGo = Invoke-Harness Go $go @('extract', $corpus)
  $digest = Assert-Equivalent $baselineRust $baselineGo 'baseline extraction'
  Assert-DocumentCount $baselineRust (Get-ExpectedDocumentCount $corpus) 'baseline Rust extraction'
  Assert-DocumentCount $baselineGo (Get-ExpectedDocumentCount $corpus) 'baseline Go extraction'
  $documents = $baselineRust.Receipt.documents
  $links = $baselineRust.Receipt.links

  $cold = @{ Rust = @(); Go = @() }
  $coldPeak = @{ Rust = @(); Go = @() }
  for ($trial = 0; $trial -lt $ColdTrials; $trial++) {
    $order = if (($trial % 2) -eq 0) { @('Rust', 'Go') } else { @('Go', 'Rust') }
    foreach ($name in $order) {
      $program = if ($name -eq 'Rust') { $rust } else { $go }
      $run = Invoke-Harness $name $program @('extract', $corpus)
      $candidate = [pscustomobject]@{ Receipt=$run.Receipt }
      $reference = if ($name -eq 'Rust') { [pscustomobject]@{ Receipt=$baselineGo.Receipt } } else { [pscustomobject]@{ Receipt=$baselineRust.Receipt } }
      Assert-Equivalent $candidate $reference "cold trial $trial $name" | Out-Null
      $cold[$name] += $run.ElapsedMs
      $coldPeak[$name] += $run.PeakWorkingSetBytes
    }
  }

  $steady = @{ Rust = @(); Go = @() }
  $steadyPeak = @{ Rust = @(); Go = @() }
  $perFormat = @{ Rust = @{}; Go = @{} }
  foreach ($name in @('Rust', 'Go')) {
    $program = if ($name -eq 'Rust') { $rust } else { $go }
    for ($iteration = 0; $iteration -lt $Iterations; $iteration++) {
      $run = Invoke-Harness $name $program @('throughput', $corpus, $WarmupPasses, $TimedPasses)
      $candidate = [pscustomobject]@{ Receipt=$run.Receipt }
      $reference = if ($name -eq 'Rust') { [pscustomobject]@{ Receipt=$baselineGo.Receipt } } else { [pscustomobject]@{ Receipt=$baselineRust.Receipt } }
      Assert-Equivalent $candidate $reference "throughput iteration $iteration $name" | Out-Null
      $steady[$name] += ($documents / [double]$run.Receipt.median_seconds)
      $steadyPeak[$name] += $run.PeakWorkingSetBytes
    }
    foreach ($format in @('md', 'html', 'txt', 'docx', 'xlsx', 'pptx', 'pdf')) {
      $samples = @()
      $baselineFormatRust = Invoke-Harness Rust $rust @('extract', $corpus, $format)
      $baselineFormatGo = Invoke-Harness Go $go @('extract', $corpus, $format)
      $expectedFormatDocuments = Get-ExpectedDocumentCount $corpus $format
      Assert-DocumentCount $baselineFormatRust $expectedFormatDocuments "per-format $format Rust baseline"
      Assert-DocumentCount $baselineFormatGo $expectedFormatDocuments "per-format $format Go baseline"
      Assert-Equivalent $baselineFormatRust $baselineFormatGo "per-format $format baseline" | Out-Null
      for ($iteration = 0; $iteration -lt $Iterations; $iteration++) {
        $run = Invoke-Harness $name $program @('throughput', $corpus, $WarmupPasses, $TimedPasses, $format)
        Assert-DocumentCount $run $expectedFormatDocuments "per-format $format throughput iteration $iteration $name"
        $samples += ($run.Receipt.documents / [double]$run.Receipt.median_seconds)
      }
      $ordered = $samples | Sort-Object
      $perFormat[$name][$format] = [pscustomobject]@{
        median_docs_per_second = [Math]::Round((Get-Median $ordered), 2)
        min_docs_per_second = [Math]::Round($ordered[0], 2)
        max_docs_per_second = [Math]::Round($ordered[-1], 2)
      }
    }
  }

  $data = foreach ($name in @('Rust', 'Go')) {
    $throughput = $steady[$name] | Sort-Object
    $coldValues = $cold[$name] | Sort-Object
    [pscustomobject]@{
      implementation = $name
      documents = $documents
      links = $links
      records_digest = $digest
      formats = $baselineRust.Receipt.formats
      per_format_throughput = $perFormat[$name]
      throughput_median_docs_per_second = [Math]::Round((Get-Median $throughput), 2)
      throughput_min_docs_per_second = [Math]::Round($throughput[0], 2)
      throughput_max_docs_per_second = [Math]::Round($throughput[-1], 2)
      throughput_peak_working_set_mib = [Math]::Round((($steadyPeak[$name] | Measure-Object -Maximum).Maximum / 1MB), 2)
      cold_completion_median_ms = [Math]::Round((Get-Median $coldValues), 0)
      cold_completion_min_ms = [Math]::Round($coldValues[0], 0)
      cold_completion_max_ms = [Math]::Round($coldValues[-1], 0)
      cold_peak_working_set_mib = [Math]::Round((($coldPeak[$name] | Measure-Object -Maximum).Maximum / 1MB), 2)
      iterations = $Iterations
      cold_trials = $ColdTrials
      warmup_passes = $WarmupPasses
      timed_passes = $TimedPasses
    }
  }
  $data | ConvertTo-Json -Depth 8 | Set-Content (Join-Path $results 'results.json')
  Get-ComputerInfo | Select-Object OsName, OsVersion, OsBuildNumber, CsProcessors, CsTotalPhysicalMemory | ConvertTo-Json | Set-Content (Join-Path $results 'machine.json')
  $data | Format-Table -AutoSize
} finally { Pop-Location }

param([int]$Iterations = 9)
$ErrorActionPreference = 'Stop'
$root = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$corpus = Join-Path $root 'bench/corpus'
$results = Join-Path $root 'bench/results'
New-Item -ItemType Directory -Force $results | Out-Null
Push-Location $root
try {
  cargo run --quiet --release --manifest-path bench/rust-harness/Cargo.toml -- generate $corpus
  cargo build --quiet --release --manifest-path bench/rust-harness/Cargo.toml
  go -C bench/go-port build -o stalelink-go.exe .
  $rust = Join-Path $root 'bench/rust-harness/target/release/stalelink-bench-harness.exe'
  $go = Join-Path $root 'bench/go-port/stalelink-go.exe'
  function Measure-Program($name, $program) {
    $watch = [Diagnostics.Stopwatch]::StartNew()
    $coldProcess = Start-Process -FilePath $program -ArgumentList @('extract', $corpus) -NoNewWindow -PassThru
    $coldProcess.WaitForExit(); $watch.Stop(); $coldProcess.Refresh()
    $coldMilliseconds = $watch.Elapsed.TotalMilliseconds
    & $program extract $corpus | Out-Null # warmup, excluded from steady-state timing
    $throughput = @(); $peak = @()
    for ($i = 0; $i -lt $Iterations; $i++) {
      $watch = [Diagnostics.Stopwatch]::StartNew(); $process = Start-Process -FilePath $program -ArgumentList @('extract', $corpus) -NoNewWindow -PassThru
      $runPeak = 0
      while (!$process.HasExited) { $process.Refresh(); $runPeak = [Math]::Max($runPeak, $process.WorkingSet64); Start-Sleep -Milliseconds 10 }
      $process.WaitForExit(); $watch.Stop(); $process.Refresh()
      $throughput += 21 / $watch.Elapsed.TotalSeconds; $peak += $runPeak
    }
    [pscustomobject]@{ implementation=$name; documents=21; links=4270; median_docs_per_second=[Math]::Round(($throughput | Sort-Object)[[int]($Iterations / 2)],2); cold_start_ms=[Math]::Round($coldMilliseconds,2); peak_rss_mib=[Math]::Round((($peak | Measure-Object -Maximum).Maximum / 1MB),2); iterations=$Iterations }
  }
  $data = @(Measure-Program Rust $rust; Measure-Program Go $go)
  $data | ConvertTo-Json | Set-Content (Join-Path $results 'results.json')
  Get-ComputerInfo | Select-Object OsName,OsVersion,OsBuildNumber,CsProcessors,CsTotalPhysicalMemory | ConvertTo-Json | Set-Content (Join-Path $results 'machine.json')
  $data | Format-Table -AutoSize
} finally { Pop-Location }

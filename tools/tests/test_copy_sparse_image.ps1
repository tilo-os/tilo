$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$tool = Join-Path (Split-Path $PSScriptRoot -Parent) 'copy-sparse-image.ps1'
$qemuRoot = Join-Path $env:USERPROFILE 'msys64\clangarm64'
$qemuImg = Join-Path $qemuRoot 'bin\qemu-img.exe'
if (-not (Test-Path -LiteralPath $qemuImg -PathType Leaf)) {
    Write-Host 'SKIP native ARM64 qemu-img is not installed'
    exit 0
}

$root = Join-Path ([IO.Path]::GetTempPath()) "tilo-sparse-copy-test-$PID"
New-Item -ItemType Directory -Path $root | Out-Null
try {
    $source = Join-Path $root 'source.raw'
    $destination = Join-Path $root 'destination.raw'
    & $qemuImg create -f raw $source 64M | Out-Null
    if ($LASTEXITCODE -ne 0) { throw 'fixture creation failed' }
    $stream = [IO.File]::Open($source, 'Open', 'Write', 'None')
    try {
        $stream.Position = 512
        $bytes = [Text.Encoding]::ASCII.GetBytes('EFI PART')
        $stream.Write($bytes, 0, $bytes.Length)
        $stream.Position = 48MB
        $stream.WriteByte(0x5a)
    } finally {
        $stream.Dispose()
    }

    & $tool -SourcePath $source -DestinationPath $destination -QemuRoot $qemuRoot -ExpectedBytes 64MB
    if ($LASTEXITCODE -ne 0) { throw 'copy tool failed' }
    if (-not (Test-Path -LiteralPath $destination -PathType Leaf)) { throw 'destination missing' }
    if (-not (Test-Path -LiteralPath "$destination.handoff.json" -PathType Leaf)) { throw 'receipt missing' }
    $receipt = Get-Content -Raw -LiteralPath "$destination.handoff.json" | ConvertFrom-Json
    if ($receipt.verification -ne 'qemu-img-compare') { throw 'receipt lacks byte verification' }
    if ([long]$receipt.logicalBytes -ne 64MB) { throw 'receipt has wrong logical size' }
    if ([long]$receipt.allocatedBytes -ge 64MB) { throw 'copy lost sparsity' }

    $failed = $false
    try {
        & $tool -SourcePath $source -DestinationPath $destination -QemuRoot $qemuRoot -ExpectedBytes 64MB 2>$null
    } catch {
        $failed = $true
    }
    if (-not $failed) { throw 'existing verified destination was replaced' }
    & $tool -SourcePath $source -DestinationPath $destination -QemuRoot $qemuRoot -ExpectedBytes 64MB -Replace
    if ($LASTEXITCODE -ne 0) { throw 'explicit atomic replacement failed' }
    Write-Host 'PASS native sparse copy is byte-identical, sparse, receipted and guarded on replacement'
} finally {
    Remove-Item -LiteralPath $root -Recurse -Force -ErrorAction SilentlyContinue
}

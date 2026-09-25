$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$tool = Join-Path (Split-Path $PSScriptRoot -Parent) 'launch-arm64-disk.ps1'
$tokens = $null
$errors = $null
[void][Management.Automation.Language.Parser]::ParseFile($tool, [ref]$tokens, [ref]$errors)
if ($errors.Count -ne 0) {
    throw "launch-arm64-disk.ps1 has syntax errors: $($errors.Message -join '; ')"
}

$source = Get-Content -Raw -LiteralPath $tool
foreach ($contract in @(
    '[switch]$WritableSourceDisk',
    '$WritableSourceDisk -and $targetIsRequested',
    'Test-QemuUsesPath $DiskPath',
    'Assert-FileIsExclusivelyWritable $DiskPath',
    'snapshot=on',
    "'writable persistent'"
)) {
    if (-not $source.Contains($contract)) {
        throw "launch-arm64-disk.ps1 is missing writable-source safety contract: $contract"
    }
}

foreach ($contract in @(
    '[int]$SerialTcpPort',
    "'SerialTcpPort must differ from QmpPort.'",
    'tcp:127.0.0.1:$SerialTcpPort,server=on,wait=off',
    "'-serial', `$serialEndpoint"
)) {
    if (-not $source.Contains($contract)) {
        throw "launch-arm64-disk.ps1 is missing interactive serial contract: $contract"
    }
}

$sshContracts = @(
    '[int]$SshHostPort',
    "'SshHostPort must differ from QmpPort and SerialTcpPort.'",
    'hostfwd=tcp:127.0.0.1:$SshHostPort-:22'
)
foreach ($contract in $sshContracts) {
    if (-not $source.Contains($contract)) {
        throw "launch-arm64-disk.ps1 is missing SSH forwarding contract: $contract"
    }
}

$qemuRoot = Join-Path $env:USERPROFILE 'msys64\clangarm64'
$qemu = Join-Path $qemuRoot 'bin\qemu-system-aarch64.exe'
if (-not (Test-Path -LiteralPath $qemu -PathType Leaf)) {
    Write-Host 'PASS static writable-source launcher contracts; SKIP behavior because native ARM64 QEMU is unavailable'
    exit 0
}

$root = Join-Path ([IO.Path]::GetTempPath()) "tilo-arm64-launcher-test-$PID"
New-Item -ItemType Directory -Path $root | Out-Null
try {
    $disk = Join-Path $root 'source.raw'
    $state = Join-Path $root 'state'
    $target = Join-Path $root 'target.raw'
    $stream = [IO.File]::Open($disk, 'CreateNew', 'ReadWrite', 'None')
    try {
        $stream.SetLength(64MB)
        $stream.Position = 512
        $gpt = [Text.Encoding]::ASCII.GetBytes('EFI PART')
        $stream.Write($gpt, 0, $gpt.Length)
    } finally {
        $stream.Dispose()
    }

    & $tool -DiskPath $disk -StateDirectory $state -QemuRoot $qemuRoot -QmpPort 45231 -CheckOnly
    if ($LASTEXITCODE -ne 0) {
        throw 'Default check-only mode did not retain the snapshot-safe source contract.'
    }
    & $tool -DiskPath $disk -StateDirectory $state -QemuRoot $qemuRoot -QmpPort 45232 -WritableSourceDisk -CheckOnly
    if ($LASTEXITCODE -ne 0) {
        throw 'Writable check-only mode did not report the explicit persistent source contract.'
    }

    $rejected = $false
    try {
        & $tool -DiskPath $disk -InstallTargetDisk $target -WritableSourceDisk -CheckOnly 2>$null
    } catch {
        $rejected = $_.Exception.Message -match 'cannot be combined'
    }
    if (-not $rejected) {
        throw 'Writable source mode accepted a simultaneous installer target.'
    }

    # Share only reads so the writable-mode probe cannot obtain its required
    # exclusive read/write handle, even within this test process.
    $lock = [IO.File]::Open($disk, 'Open', 'Read', 'Read')
    try {
        $rejected = $false
        try {
            & $tool -DiskPath $disk -StateDirectory $state -QemuRoot $qemuRoot -QmpPort 45233 -WritableSourceDisk -CheckOnly 2>$null
        } catch {
            $rejected = $_.Exception.Message -match 'cannot be opened exclusively'
        }
        if (-not $rejected) {
            throw 'Writable source mode accepted a disk with a conflicting file handle.'
        }
    } finally {
        $lock.Dispose()
    }
    Write-Host 'PASS writable-source mode is opt-in, exclusive, and incompatible with installer targets'
} finally {
    Remove-Item -LiteralPath $root -Recurse -Force -ErrorAction SilentlyContinue
}

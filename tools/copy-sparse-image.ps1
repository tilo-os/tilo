<#
.SYNOPSIS
Copies a raw image into a verified sparse file on native Windows.

.DESCRIPTION
The source may be a read-only WSL UNC path. qemu-img writes directly to NTFS,
compares every guest-visible byte with the source, and only then atomically
promotes the temporary file. This deliberately avoids NBD and qcow2 backing
chains across the WSL/Windows boundary.
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory)] [string]$SourcePath,
    [Parameter(Mandatory)] [string]$DestinationPath,
    [string]$QemuRoot = (Join-Path $env:USERPROFILE 'msys64\clangarm64'),
    [ValidateRange(1, [long]::MaxValue)] [long]$ExpectedBytes = 68719476736,
    [switch]$Replace
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

function Invoke-QemuImg {
    param([string[]]$Arguments, [string]$Failure)
    & $script:QemuImg @Arguments
    if ($LASTEXITCODE -ne 0) {
        throw "$Failure (qemu-img status $LASTEXITCODE)"
    }
}

if (-not ('Tilo.NativeFileAllocation' -as [type])) {
    Add-Type -TypeDefinition @'
using System;
using System.ComponentModel;
using System.IO;
using System.Runtime.InteropServices;
using Microsoft.Win32.SafeHandles;
namespace Tilo { public static class NativeFileAllocation {
    [StructLayout(LayoutKind.Sequential)]
    struct FileStandardInfo {
        public long AllocationSize;
        public long EndOfFile;
        public uint NumberOfLinks;
        [MarshalAs(UnmanagedType.Bool)] public bool DeletePending;
        [MarshalAs(UnmanagedType.Bool)] public bool Directory;
    }
    [DllImport("kernel32.dll", SetLastError=true)]
    static extern bool GetFileInformationByHandleEx(SafeFileHandle handle,
        int informationClass, out FileStandardInfo information, uint size);
    public static long Bytes(string path) {
        FileStream stream = File.Open(path, FileMode.Open, FileAccess.Read,
                                      FileShare.Read | FileShare.Delete);
        try {
            FileStandardInfo information;
            if (!GetFileInformationByHandleEx(stream.SafeFileHandle, 1,
                    out information, (uint)Marshal.SizeOf(typeof(FileStandardInfo))))
                throw new Win32Exception(Marshal.GetLastWin32Error());
            return information.AllocationSize;
        } finally { stream.Dispose(); }
    }
} }
'@
}

$script:QemuImg = Join-Path $QemuRoot 'bin\qemu-img.exe'
if (-not (Test-Path -LiteralPath $script:QemuImg -PathType Leaf)) {
    throw "Native qemu-img is missing: $script:QemuImg"
}
if (-not (Test-Path -LiteralPath $SourcePath -PathType Leaf)) {
    throw "Source image is missing: $SourcePath"
}

$sourceFull = [IO.Path]::GetFullPath($SourcePath)
$destinationFull = [IO.Path]::GetFullPath($DestinationPath)
if ($sourceFull.Equals($destinationFull, [StringComparison]::OrdinalIgnoreCase)) {
    throw 'Source and destination must differ.'
}
if ((Test-Path -LiteralPath $destinationFull) -and -not $Replace) {
    throw "Refusing to replace an existing verified image: $destinationFull"
}

$destinationDirectory = Split-Path -Parent $destinationFull
New-Item -ItemType Directory -Path $destinationDirectory -Force | Out-Null
$temporary = "$destinationFull.incoming.$PID"
$receipt = "$destinationFull.handoff.json"
if ((Test-Path -LiteralPath $temporary) -or ((Test-Path -LiteralPath $receipt) -and -not $Replace)) {
    throw 'A handoff temporary or receipt path already exists.'
}

try {
    $sourceInfoRaw = (& $script:QemuImg info --output=json -f raw $sourceFull) -join "`n"
    if ($LASTEXITCODE -ne 0) { throw 'qemu-img could not inspect the source image.' }
    $sourceInfo = $sourceInfoRaw | ConvertFrom-Json
    if ([long]$sourceInfo.'virtual-size' -ne $ExpectedBytes) {
        throw "Unexpected source size $($sourceInfo.'virtual-size'); expected $ExpectedBytes bytes."
    }

    Invoke-QemuImg -Arguments @('convert', '-f', 'raw', '-O', 'raw', '-S', '4k', $sourceFull, $temporary) `
        -Failure 'Sparse image conversion failed'

    $copyInfoRaw = (& $script:QemuImg info --output=json -f raw $temporary) -join "`n"
    if ($LASTEXITCODE -ne 0) { throw 'qemu-img could not inspect the copied image.' }
    $copyInfo = $copyInfoRaw | ConvertFrom-Json
    if ([long]$copyInfo.'virtual-size' -ne $ExpectedBytes) {
        throw "Copied image has unexpected size $($copyInfo.'virtual-size')."
    }
    $flush = [IO.File]::Open($temporary, 'Open', 'ReadWrite', 'Read')
    try { $flush.Flush($true) } finally { $flush.Dispose() }
    # qemu-img's raw driver reports actual-size=0 on Windows. Ask NTFS for the
    # real allocated size instead; this also proves the output stayed sparse.
    $allocatedBytes = [Tilo.NativeFileAllocation]::Bytes($temporary)
    if ($allocatedBytes -le 0 -or $allocatedBytes -ge $ExpectedBytes) {
        throw "Copied image is empty or unexpectedly dense: $allocatedBytes allocated bytes."
    }

    # Compare guest-visible bytes, including nonzero data beyond the GPT. Do
    # not require identical allocation maps: ext4 and NTFS represent sparse
    # extents differently even when every disk byte is identical.
    Invoke-QemuImg -Arguments @('compare', '-f', 'raw', '-F', 'raw', $sourceFull, $temporary) `
        -Failure 'Copied image differs from its read-only source'

    $stream = [IO.File]::Open($temporary, 'Open', 'Read', 'Read')
    try {
        $header = [byte[]]::new(8)
        $stream.Position = 512
        if ($stream.Read($header, 0, 8) -ne 8 -or [Text.Encoding]::ASCII.GetString($header) -ne 'EFI PART') {
            throw 'Copied image has no primary GPT header.'
        }
    } finally {
        $stream.Dispose()
    }

    Move-Item -LiteralPath $temporary -Destination $destinationFull -Force
    $document = [ordered]@{
        schemaVersion = 1
        purpose = 'tilo-local-sparse-image-handoff'
        verifiedUtc = [DateTime]::UtcNow.ToString('o')
        source = $sourceFull
        destination = $destinationFull
        logicalBytes = $ExpectedBytes
        allocatedBytes = $allocatedBytes
        verification = 'qemu-img-compare'
        sourceReadOnlyRequired = $true
        qemuImg = $script:QemuImg
    }
    $document | ConvertTo-Json | Set-Content -LiteralPath $receipt -Encoding UTF8
    Write-Host "READY disk=$destinationFull logical_bytes=$ExpectedBytes allocated_bytes=$allocatedBytes verification=qemu-img-compare"
} catch {
    Remove-Item -LiteralPath $temporary -Force -ErrorAction SilentlyContinue
    throw
}

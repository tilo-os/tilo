param(
    [Parameter(Mandatory = $true)][ValidateSet("broker", "signer")][string]$Kind,
    [Parameter(Mandatory = $true)][string]$Path
)

$ErrorActionPreference = "Stop"

function Assert-qOSTrustedExecutable([string]$Candidate, [string]$Description) {
    if (-not [IO.Path]::IsPathFullyQualified($Candidate)) { throw "$Description must be absolute" }
    $Item = Get-Item -LiteralPath $Candidate -Force
    if ($Item.PSIsContainer -or ($Item.Attributes -band [IO.FileAttributes]::ReparsePoint)) {
        throw "$Description must be a regular non-reparse-point file"
    }
    if ($Item.FullName -cne $Candidate) { throw "$Description must use its canonical path" }
    foreach ($UntrustedRoot in @($env:GITHUB_WORKSPACE, $env:RUNNER_TEMP)) {
        if ($UntrustedRoot -and $Item.FullName.StartsWith(
                ([IO.Path]::GetFullPath($UntrustedRoot).TrimEnd('\') + '\'),
                [StringComparison]::OrdinalIgnoreCase)) {
            throw "$Description must not come from the checkout or job temporary directory"
        }
    }
    $Acl = Get-Acl -LiteralPath $Item.FullName
    if ($Acl.Owner -notmatch '(?i)(SYSTEM|Administrators)$') {
        throw "$Description must be owned by SYSTEM or Administrators"
    }
    $Dangerous = ([Security.AccessControl.FileSystemRights]::Write -bor
        [Security.AccessControl.FileSystemRights]::Modify -bor
        [Security.AccessControl.FileSystemRights]::FullControl)
    foreach ($Rule in $Acl.Access) {
        if ($Rule.AccessControlType -eq 'Allow' -and ($Rule.FileSystemRights -band $Dangerous) -and
            $Rule.IdentityReference.Value -match '(?i)(Everyone|Authenticated Users|BUILTIN\\Users|INTERACTIVE)$') {
            throw "$Description ACL permits an untrusted principal to replace it"
        }
    }
    return $Item.FullName
}

if (-not $env:RUNNER_TEMP -or -not $env:GITHUB_WORKSPACE) { throw "runner paths are required" }
$Trusted = Assert-qOSTrustedExecutable $Path $Kind
if ($Kind -eq "signer") {
    "QOS_WINDOWS_SIGNER=$Trusted"
    exit 0
}
if (-not $env:ACTIONS_ID_TOKEN_REQUEST_URL -or -not $env:ACTIONS_ID_TOKEN_REQUEST_TOKEN) {
    throw "Forgejo OIDC request credentials are required"
}
$LeaseDirectory = Join-Path $env:RUNNER_TEMP ("qos-imager-windows-" + [Guid]::NewGuid().ToString('N'))
$Lease = Join-Path $LeaseDirectory "lease.env"
New-Item -ItemType Directory -Path $LeaseDirectory | Out-Null
try {
    if (Test-Path -LiteralPath $Lease) { throw "credential lease output already exists" }
    & $Trusted windows $Lease
    if ($LASTEXITCODE -ne 0) { throw "credential broker failed" }
    $Item = Get-Item -LiteralPath $Lease -Force
    if ($Item.PSIsContainer -or ($Item.Attributes -band [IO.FileAttributes]::ReparsePoint)) {
        throw "credential broker must create a regular non-reparse-point lease"
    }
    $Acl = Get-Acl -LiteralPath $Lease
    if (-not $Acl.AreAccessRulesProtected) { throw "credential lease must disable inherited ACLs" }
    $CurrentIdentity = [Security.Principal.WindowsIdentity]::GetCurrent().Name
    if ($Acl.Owner -cne $CurrentIdentity) { throw "credential lease must be owned by the runner identity" }
    foreach ($Rule in $Acl.Access) {
        $Principal = $Rule.IdentityReference.Value
        $TrustedPrincipal = $Principal -ceq $CurrentIdentity -or $Principal -match '(?i)(SYSTEM|Administrators)$'
        if ($Rule.AccessControlType -eq 'Allow' -and -not $TrustedPrincipal) {
            throw "credential lease ACL grants another principal access"
        }
    }
    $Bytes = [IO.File]::ReadAllBytes($Lease)
    if ($Bytes.Length -eq 0 -or $Bytes -contains 0 -or $Bytes -contains 13) {
        throw "credential broker returned malformed bytes"
    }
    $Text = [Text.UTF8Encoding]::new($false, $true).GetString($Bytes)
    $Lines = $Text -split "`n"
    if ($Lines[-1] -eq '') { $Lines = $Lines[0..($Lines.Count - 2)] }
    if ($Lines.Count -ne 1 -or $Lines[0] -notmatch '^QOS_WINDOWS_SIGNER=([^\x00-\x1f\x7f]+)$') {
        throw "Windows broker must return exactly one signer path"
    }
    $Signer = Assert-qOSTrustedExecutable $Matches[1] "Windows signer"
    "QOS_WINDOWS_SIGNER=$Signer"
} finally {
    Remove-Item -LiteralPath $Lease -Force -ErrorAction SilentlyContinue
    Remove-Item -LiteralPath $LeaseDirectory -Force -ErrorAction SilentlyContinue
}

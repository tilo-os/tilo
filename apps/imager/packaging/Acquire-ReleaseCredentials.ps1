param(
    [Parameter(Mandatory = $true)][ValidateSet("broker", "signer")][string]$Kind,
    [Parameter(Mandatory = $true)][string]$Path
)

$ErrorActionPreference = "Stop"
$TiloTrustedOwnerSids = @(
    'S-1-5-18',
    'S-1-5-32-544',
    'S-1-5-80-956008885-3418522649-1831038044-1853292631-2271478464'
)

function Assert-TiloTrustedExecutable([string]$Candidate, [string]$Description) {
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
    $Current = $Item
    while ($Current) {
        $Acl = Get-Acl -LiteralPath $Current.FullName
        $OwnerSid = $Acl.GetOwner([Security.Principal.SecurityIdentifier]).Value
        if ($OwnerSid -notin $TiloTrustedOwnerSids) {
            throw "$Description and every parent must have a trusted owner"
        }
        # Directory WriteData may only create an unrelated child. Reject rights
        # that can replace this component or change its ACL; reject all writes
        # on the executable itself.
        $Dangerous = ([Security.AccessControl.FileSystemRights]::Modify -bor
            [Security.AccessControl.FileSystemRights]::FullControl -bor
            [Security.AccessControl.FileSystemRights]::Delete -bor
            [Security.AccessControl.FileSystemRights]::DeleteSubdirectoriesAndFiles -bor
            [Security.AccessControl.FileSystemRights]::ChangePermissions -bor
            [Security.AccessControl.FileSystemRights]::TakeOwnership)
        if ($Current -is [IO.FileInfo]) {
            $Dangerous = $Dangerous -bor [Security.AccessControl.FileSystemRights]::Write
        }
        foreach ($Rule in @($Acl.GetAccessRules(
                    $true, $true, [Security.Principal.SecurityIdentifier]))) {
            $TrustedPrincipal = $Rule.IdentityReference.Value -in $TiloTrustedOwnerSids
            if ($Rule.AccessControlType -eq 'Allow' -and
                ($Rule.FileSystemRights -band $Dangerous) -and -not $TrustedPrincipal) {
                throw "$Description or a parent is writable by an untrusted principal"
            }
        }
        $Current = if ($Current -is [IO.FileInfo]) { $Current.Directory } else { $Current.Parent }
    }
    return $Item.FullName
}

if (-not $env:RUNNER_TEMP -or -not $env:GITHUB_WORKSPACE) { throw "runner paths are required" }
$Trusted = Assert-TiloTrustedExecutable $Path $Kind
if ($Kind -eq "signer") {
    "TILO_WINDOWS_SIGNER=$Trusted"
    exit 0
}
if (-not $env:ACTIONS_ID_TOKEN_REQUEST_URL -or -not $env:ACTIONS_ID_TOKEN_REQUEST_TOKEN) {
    throw "Forgejo OIDC request credentials are required"
}
$LeaseDirectory = Join-Path $env:RUNNER_TEMP ("tilo-imager-windows-" + [Guid]::NewGuid().ToString('N'))
$Lease = Join-Path $LeaseDirectory "lease.env"
New-Item -ItemType Directory -Path $LeaseDirectory | Out-Null
try {
    if (Test-Path -LiteralPath $Lease) { throw "credential lease output already exists" }
    $null = & $Trusted windows $Lease
    if ($LASTEXITCODE -ne 0) { throw "credential broker failed" }
    $Item = Get-Item -LiteralPath $Lease -Force
    if ($Item.PSIsContainer -or ($Item.Attributes -band [IO.FileAttributes]::ReparsePoint)) {
        throw "credential broker must create a regular non-reparse-point lease"
    }
    $Acl = Get-Acl -LiteralPath $Lease
    if (-not $Acl.AreAccessRulesProtected) { throw "credential lease must disable inherited ACLs" }
    $CurrentIdentitySid = [Security.Principal.WindowsIdentity]::GetCurrent().User.Value
    if ($Acl.GetOwner([Security.Principal.SecurityIdentifier]).Value -cne $CurrentIdentitySid) {
        throw "credential lease must be owned by the runner identity"
    }
    foreach ($Rule in @($Acl.GetAccessRules(
                $true, $true, [Security.Principal.SecurityIdentifier]))) {
        $Principal = $Rule.IdentityReference.Value
        $TrustedPrincipal = $Principal -ceq $CurrentIdentitySid -or
            $Principal -in $TiloTrustedOwnerSids
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
    if ($Lines.Count -ne 1 -or $Lines[0] -notmatch '^TILO_WINDOWS_SIGNER=([^\x00-\x1f\x7f]+)$') {
        throw "Windows broker must return exactly one signer path"
    }
    $Signer = Assert-TiloTrustedExecutable $Matches[1] "Windows signer"
    "TILO_WINDOWS_SIGNER=$Signer"
} finally {
    Remove-Item -LiteralPath $Lease -Force -ErrorAction SilentlyContinue
    Remove-Item -LiteralPath $LeaseDirectory -Force -ErrorAction SilentlyContinue
}

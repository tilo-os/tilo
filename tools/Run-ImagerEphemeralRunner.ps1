param(
    [Parameter(Mandatory = $true)][string]$Label,
    [Parameter(Mandatory = $true)][string]$UuidFile,
    [Parameter(Mandatory = $true)][string]$TokenFile
)

# Install this script and forgejo-runner.exe as Administrator-owned files on a
# disposable golden image. Never execute a copy from a workflow checkout.
$ErrorActionPreference = "Stop"
$Runner = if ($env:QOS_FORGEJO_RUNNER) {
    $env:QOS_FORGEJO_RUNNER
} else {
    Join-Path $env:ProgramFiles "Forgejo Runner\forgejo-runner.exe"
}
$Server = if ($env:QOS_FORGEJO_URL) { $env:QOS_FORGEJO_URL } else { "https://code.sandbox.example" }
$MaxWaitText = if ($env:QOS_RUNNER_MAX_WAIT_SECONDS) { $env:QOS_RUNNER_MAX_WAIT_SECONDS } else { "21600" }

function Assert-qOSPrivateMetadataFile([string]$Path, [string]$Description) {
    if (-not [IO.Path]::IsPathFullyQualified($Path)) { throw "$Description must be absolute" }
    $Item = Get-Item -LiteralPath $Path -Force
    if ($Item.PSIsContainer -or ($Item.Attributes -band [IO.FileAttributes]::ReparsePoint)) {
        throw "$Description must be a regular non-reparse-point file"
    }
    if ($Item.FullName -cne [IO.Path]::GetFullPath($Path)) { throw "$Description must use its canonical path" }
    $Acl = Get-Acl -LiteralPath $Item.FullName
    $Identity = [Security.Principal.WindowsIdentity]::GetCurrent().Name
    if (-not $Acl.AreAccessRulesProtected -or $Acl.Owner -cne $Identity) {
        throw "$Description must disable inheritance and be owned by the runner identity"
    }
    foreach ($Rule in $Acl.Access) {
        $Trusted = $Rule.IdentityReference.Value -ceq $Identity -or
            $Rule.IdentityReference.Value -match '(?i)(SYSTEM|Administrators)$'
        if ($Rule.AccessControlType -eq 'Allow' -and -not $Trusted) {
            throw "$Description ACL grants another principal access"
        }
    }
    $ParentAcl = Get-Acl -LiteralPath $Item.Directory.FullName
    if (-not $ParentAcl.AreAccessRulesProtected -or $ParentAcl.Owner -cne $Identity) {
        throw "$Description parent must be a private runner-owned directory"
    }
    $CanDeleteChildren = $false
    foreach ($Rule in $ParentAcl.Access) {
        $IsRunner = $Rule.IdentityReference.Value -ceq $Identity
        $Trusted = $IsRunner -or $Rule.IdentityReference.Value -match '(?i)(SYSTEM|Administrators)$'
        if ($Rule.AccessControlType -eq 'Allow' -and -not $Trusted) {
            throw "$Description parent ACL grants another principal access"
        }
        $DeleteRights = ([Security.AccessControl.FileSystemRights]::Modify -bor
            [Security.AccessControl.FileSystemRights]::FullControl -bor
            [Security.AccessControl.FileSystemRights]::DeleteSubdirectoriesAndFiles)
        if ($IsRunner -and $Rule.AccessControlType -eq 'Allow' -and
            ($Rule.FileSystemRights -band $DeleteRights)) {
            $CanDeleteChildren = $true
        }
    }
    if (-not $CanDeleteChildren) { throw "$Description parent must let the runner erase registration files" }
    return $Item
}

function Assert-qOSTrustedRunner([string]$Path) {
    if (-not [IO.Path]::IsPathFullyQualified($Path)) { throw "forgejo-runner must be absolute" }
    $Item = Get-Item -LiteralPath $Path -Force
    if ($Item.PSIsContainer -or ($Item.Attributes -band [IO.FileAttributes]::ReparsePoint)) {
        throw "forgejo-runner must be a regular non-reparse-point file"
    }
    if ($Item.FullName -cne [IO.Path]::GetFullPath($Path)) { throw "forgejo-runner must use its canonical path" }
    $Current = $Item
    while ($Current) {
        $Acl = Get-Acl -LiteralPath $Current.FullName
        if ($Acl.Owner -notmatch '(?i)(SYSTEM|Administrators|TrustedInstaller)$') {
            throw "forgejo-runner and every parent must have a trusted owner"
        }
        # WriteData on a drive root may only permit creating unrelated children.
        # Reject rights which can replace this path component or change its ACL.
        $Dangerous = ([Security.AccessControl.FileSystemRights]::Modify -bor
            [Security.AccessControl.FileSystemRights]::FullControl -bor
            [Security.AccessControl.FileSystemRights]::Delete -bor
            [Security.AccessControl.FileSystemRights]::DeleteSubdirectoriesAndFiles -bor
            [Security.AccessControl.FileSystemRights]::ChangePermissions -bor
            [Security.AccessControl.FileSystemRights]::TakeOwnership)
        if ($Current -is [IO.FileInfo]) {
            $Dangerous = $Dangerous -bor [Security.AccessControl.FileSystemRights]::Write
        }
        foreach ($Rule in $Acl.Access) {
            $TrustedPrincipal = $Rule.IdentityReference.Value -match '(?i)(SYSTEM|Administrators|TrustedInstaller)$'
            if ($Rule.AccessControlType -eq 'Allow' -and ($Rule.FileSystemRights -band $Dangerous) -and
                -not $TrustedPrincipal) {
                throw "forgejo-runner or a parent is writable by an untrusted principal"
            }
        }
        $Current = if ($Current -is [IO.FileInfo]) { $Current.Directory } else { $Current.Parent }
    }
    return $Item.FullName
}

$UuidItem = Assert-qOSPrivateMetadataFile $UuidFile "runner UUID file"
$TokenItem = Assert-qOSPrivateMetadataFile $TokenFile "runner token file"
$SafeMetadata = @($UuidItem.FullName, $TokenItem.FullName)
$Process = $null
try {
    if ($UuidItem.FullName -ceq $TokenItem.FullName) { throw "UUID and token paths must differ" }
    if ($Server -notin @("https://code.sandbox.example", "https://code.sandbox.example/")) {
        throw "Forgejo URL must be the pinned qOS origin"
    }
    $MaxWait = [int]::Parse($MaxWaitText, [Globalization.CultureInfo]::InvariantCulture)
    if ($MaxWait -lt 1 -or $MaxWait -gt 86400) {
        throw "runner wait timeout must be between 1 and 86400 seconds"
    }
    if (-not [Environment]::Is64BitOperatingSystem -or
        [Runtime.InteropServices.RuntimeInformation]::OSArchitecture -ne [Runtime.InteropServices.Architecture]::X64 -or
        $Label -cne "imager-windows-x86_64") {
        throw "runner label imager-windows-x86_64 requires native Windows x86-64"
    }

    $UuidBytes = [IO.File]::ReadAllBytes($UuidItem.FullName)
    $TokenBytes = [IO.File]::ReadAllBytes($TokenItem.FullName)
    $Utf8 = [Text.UTF8Encoding]::new($false, $true)
    $Uuid = $Utf8.GetString($UuidBytes)
    $Token = $Utf8.GetString($TokenBytes)
    if ($Uuid -notmatch '^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$') {
        throw "runner metadata contains an invalid UUID"
    }
    if ($Token -notmatch '^[0-9a-fA-F]{40}$') {
        throw "runner metadata contains an invalid one-job token"
    }
    Remove-Variable Token, TokenBytes -ErrorAction SilentlyContinue

    $TrustedRunner = Assert-qOSTrustedRunner $Runner
    $VersionText = (& $TrustedRunner --version 2>&1 | Out-String).Trim()
    if ($LASTEXITCODE -ne 0 -or $VersionText -notmatch '(?<major>[0-9]+)\.(?<minor>[0-9]+)\.(?<patch>[0-9]+)') {
        throw "could not determine forgejo-runner version"
    }
    $Major = [int]$Matches.major
    $Minor = [int]$Matches.minor
    if ($Major -lt 12 -or ($Major -eq 12 -and $Minor -lt 5)) {
        throw "forgejo-runner 12.5 or newer is required for one-job mode"
    }

    $TokenUrl = [Uri]::new($TokenItem.FullName).AbsoluteUri
    $Arguments = @(
        "one-job", "--url", $Server.TrimEnd('/'), "--uuid", $Uuid,
        "--token-url", $TokenUrl, "--label", "imager-windows-x86_64:host", "--wait"
    )
    $Process = Start-Process -FilePath $TrustedRunner -ArgumentList $Arguments -NoNewWindow -PassThru
    if (-not $Process.WaitForExit($MaxWait * 1000)) {
        & "$env:SystemRoot\System32\taskkill.exe" /PID $Process.Id /T /F | Out-Null
        if (-not $Process.HasExited) { $Process.Kill() }
        $Process.WaitForExit()
        throw "ephemeral Forgejo runner timed out waiting for its one job"
    }
    if ($Process.ExitCode -ne 0) {
        throw "ephemeral Forgejo runner exited unsuccessfully ($($Process.ExitCode))"
    }
    Write-Output "one-job runner completed; destroy this VM now"
} finally {
    $CleanupFailures = @()
    try {
        if ($Process -and -not $Process.HasExited) {
            & "$env:SystemRoot\System32\taskkill.exe" /PID $Process.Id /T /F | Out-Null
            if (-not $Process.HasExited) { $Process.Kill() }
            $Process.WaitForExit()
        }
    } catch {
        $CleanupFailures += "running process $($Process.Id)"
    }
    foreach ($Path in $SafeMetadata) {
        Remove-Item -LiteralPath $Path -Force -ErrorAction SilentlyContinue
        if (Test-Path -LiteralPath $Path) { $CleanupFailures += $Path }
    }
    if ($CleanupFailures.Count -ne 0) {
        throw "ephemeral runner cleanup failed for: $($CleanupFailures -join ', ')"
    }
}

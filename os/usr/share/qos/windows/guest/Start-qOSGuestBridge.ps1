<#
The guest endpoint is intentionally small: catalog, health and preparation of
one single-use RemoteApp credential (or a focused SPICE presentation). It is an HTTPS service bound only to the
non-forwarding qOS host-only network. Every request also needs a fresh HMAC
over timestamp, nonce and body using the bootstrap key installed SYSTEM-only.

The host pins the TLS certificate returned by the authenticated health call;
the bootstrap HMAC is an enrollment channel, not a replacement for TLS.
#>
[CmdletBinding()]
param()
$ErrorActionPreference = 'Stop'
$root = 'C:\ProgramData\qOS\RemoteApp'
$focusRoot = 'C:\ProgramData\qOS\FocusedApp'
$config = Get-Content -Raw -LiteralPath (Join-Path $root 'bridge.json') | ConvertFrom-Json
$secret = [IO.File]::ReadAllBytes((Join-Path $root 'bridge.secret'))
$appsPath = Join-Path $root 'published-applications.json'
$installerRoot = 'C:\ProgramData\qOS\InstallerSession'
$installerStateRoot = Join-Path $root 'installer-state'
$packageStateRoot = Join-Path $installerStateRoot 'packages'
$operationStateRoot = Join-Path $installerStateRoot 'operations'
$uninstallRecordsPath = Join-Path $root 'uninstall-records.json'
$maximumInstallerBytes = [UInt64]17179869184
$maximumStagedBytes = [UInt64]34359738368
$port = [int]$config.bridgePort
$guestIp = (Get-NetIPAddress -AddressFamily IPv4 | Where-Object { $_.IPAddress -like '192.168.251.*' } | Select-Object -First 1 -ExpandProperty IPAddress)
if ([string]::IsNullOrWhiteSpace($guestIp)) { throw 'qOS host-only network address is unavailable.' }
$seenNonces = [System.Collections.Concurrent.ConcurrentDictionary[string, Int64]]::new()

function Get-Hmac([string]$text) {
    $hmac = [Security.Cryptography.HMACSHA256]::new($secret)
    try { return ([BitConverter]::ToString($hmac.ComputeHash([Text.Encoding]::UTF8.GetBytes($text)))).Replace('-', '').ToLowerInvariant() }
    finally { $hmac.Dispose() }
}
function Test-FixedTime([string]$left, [string]$right) {
    $a = [Text.Encoding]::ASCII.GetBytes($left); $b = [Text.Encoding]::ASCII.GetBytes($right)
    if ($a.Length -ne $b.Length) { return $false }
    return [Security.Cryptography.CryptographicOperations]::FixedTimeEquals($a, $b)
}
function Write-Json($context, $value, [int]$status = 200) {
    $bytes = [Text.Encoding]::UTF8.GetBytes(($value | ConvertTo-Json -Depth 8 -Compress))
    $context.Response.StatusCode = $status; $context.Response.ContentType = 'application/json'
    # The initial enrollment request intentionally cannot rely on a pre-existing
    # TLS trust anchor. Bind every response to the already-authenticated
    # request so the host can verify the bridge certificate DER before it
    # persists that DER as its Rustls root. This also prevents a host-only
    # relay from swapping catalogue or credential JSON.
    $timestamp = [string]$context.Request.Headers['X-qOS-Timestamp']
    $nonce = [string]$context.Request.Headers['X-qOS-Nonce']
    if (-not [string]::IsNullOrWhiteSpace($timestamp) -and -not [string]::IsNullOrWhiteSpace($nonce)) {
        $responseText = [Text.Encoding]::UTF8.GetString($bytes)
        $context.Response.Headers.Add('X-qOS-Response-Auth', (Get-Hmac "$timestamp`n$nonce`n$responseText"))
    }
    $context.Response.ContentLength64 = $bytes.Length; $context.Response.OutputStream.Write($bytes, 0, $bytes.Length); $context.Response.Close()
}
function Require-RequestAuth($context, [string]$body) {
    $timestamp = $context.Request.Headers['X-qOS-Timestamp']; $nonce = $context.Request.Headers['X-qOS-Nonce']; $provided = $context.Request.Headers['X-qOS-Auth']
    if ([string]::IsNullOrWhiteSpace($timestamp) -or [string]::IsNullOrWhiteSpace($nonce) -or [string]::IsNullOrWhiteSpace($provided)) { throw 'missing bridge authentication headers' }
    $now = [DateTimeOffset]::UtcNow.ToUnixTimeSeconds(); $then = 0L
    if (-not [Int64]::TryParse($timestamp, [ref]$then) -or [Math]::Abs($now - $then) -gt 60) { throw 'stale bridge request' }
    if ($nonce -notmatch '^[A-Za-z0-9-]{16,128}$') { throw 'invalid bridge nonce' }
    $expected = Get-Hmac "$timestamp`n$nonce`n$body"
    if (-not (Test-FixedTime $expected $provided)) { throw 'invalid bridge authentication' }
    if (-not $seenNonces.TryAdd($nonce, $now)) { throw 'replayed bridge nonce' }
    foreach ($old in $seenNonces.GetEnumerator()) {
        if ($old.Value -lt ($now - 61)) { $discard = 0L; [void]$seenNonces.TryRemove($old.Key, [ref]$discard) }
    }
}

function Get-ManagedInteractiveUser() {
    # Win32_ComputerSystem.UserName is populated only for the active console
    # session. RDP-only/service sessions do not satisfy the SPICE contract;
    # qOS must report that honestly instead of attaching an empty desktop.
    $user = [string](Get-CimInstance -ClassName Win32_ComputerSystem).UserName
    if ([string]::IsNullOrWhiteSpace($user) -or $user -match '[\x00-\x1f\x7f]') { return $null }
    if ($user -in @('SYSTEM', 'NT AUTHORITY\SYSTEM', 'LOCAL SERVICE', 'NETWORK SERVICE')) { return $null }
    return $user
}

function Resolve-ManagedExecutable($app) {
    $path = [IO.Path]::GetFullPath([string]$app.path)
    if ($path -notmatch '^(C:\\Program Files( \(x86\))?\\|C:\\Windows\\)') {
        throw 'application is outside a managed Windows location'
    }
    if (-not (Test-Path $path -PathType Leaf)) { throw 'published application executable is missing' }
    return $path
}

function Start-FocusedApplication($app) {
    $user = Get-ManagedInteractiveUser
    if ($null -eq $user) { throw 'no managed interactive session exists for the SPICE fallback' }
    $path = Resolve-ManagedExecutable $app
    $jobs = Join-Path $focusRoot 'jobs'
    New-Item -ItemType Directory -Force -Path $jobs | Out-Null
    # The install payload is SYSTEM/admin-only. Grant only this active user
    # read/execute access to the separate focused-app directory and its job;
    # the bridge secret and application manifest remain unreadable.
    $focusAcl = Get-Acl -LiteralPath $focusRoot
    $account = New-Object System.Security.Principal.NTAccount($user)
    $focusRule = New-Object System.Security.AccessControl.FileSystemAccessRule($account, 'ReadAndExecute', 'ContainerInherit,ObjectInherit', 'None', 'Allow')
    [void]$focusAcl.AddAccessRule($focusRule)
    Set-Acl -LiteralPath $focusRoot -AclObject $focusAcl
    $id = [Guid]::NewGuid().ToString('N')
    $taskName = "qOS-Focus-$($id.Substring(0, 16))"
    $jobPath = Join-Path $jobs "$id.json"
    $job = @{ executable = $path; arguments = [string]$app.arguments; taskName = $taskName }
    $job | ConvertTo-Json -Compress | Set-Content -LiteralPath $jobPath -Encoding UTF8
    $acl = Get-Acl -LiteralPath $jobPath
    $account = New-Object System.Security.Principal.NTAccount($user)
    $rule = New-Object System.Security.AccessControl.FileSystemAccessRule($account, 'Read', 'None', 'Allow')
    [void]$acl.AddAccessRule($rule)
    Set-Acl -LiteralPath $jobPath -AclObject $acl
    $helper = Join-Path $focusRoot 'Start-qOSFocusedApp.ps1'
    $action = New-ScheduledTaskAction -Execute 'powershell.exe' -Argument "-NoLogo -NoProfile -NonInteractive -ExecutionPolicy RemoteSigned -File `"$helper`" -JobPath `"$jobPath`""
    $principal = New-ScheduledTaskPrincipal -UserId $user -LogonType InteractiveToken -RunLevel Limited
    $settings = New-ScheduledTaskSettingsSet -ExecutionTimeLimit (New-TimeSpan -Hours 12)
    try {
        Register-ScheduledTask -TaskName $taskName -Action $action -Principal $principal -Settings $settings -Force | Out-Null
        Start-ScheduledTask -TaskName $taskName
    } catch {
        Unregister-ScheduledTask -TaskName $taskName -Confirm:$false -ErrorAction SilentlyContinue
        Remove-Item -LiteralPath $jobPath -Force -ErrorAction SilentlyContinue
        throw
    }
    return @{ user = $user; taskName = $taskName; applicationId = $app.id }
}

function Get-PublishedManifest() {
    $source = Get-Content -Raw -LiteralPath $appsPath | ConvertFrom-Json
    $generationText = [string]$source.generation
    if ($generationText -notmatch '^[0-9]{1,19}$') {
        throw 'published application catalogue has an invalid generation'
    }
    try { $generation = [UInt64]$generationText }
    catch { throw 'published application catalogue generation is out of range' }
    return @{ generation = $generation; applications = @($source.applications) }
}

function Get-PublishedCatalog() {
    # The administrator-owned manifest carries executable paths and arguments,
    # but those never leave the Windows SYSTEM bridge. The host receives only
    # opaque published ids and aliases that `WindowsAppliance` validates.
    $source = Get-PublishedManifest
    $uninstallStore = Get-UninstallRecordStore
    $published = @()
    foreach ($app in @($source.applications)) {
        $canUninstall = @($uninstallStore.records | Where-Object { $_.applicationId -eq [string]$app.id -and -not [bool]$_.consumed }).Count -eq 1
        $published += @{ id = [string]$app.id; displayName = [string]$app.name; remoteAppAlias = [string]$app.id; associations = @(); uninstallAvailable = $canUninstall }
    }
    return @{ generation = $source.generation; applications = $published }
}

function Test-OpaqueId([string]$value, [int]$minimum = 1) {
    return $value.Length -ge $minimum -and $value -match '^[A-Za-z0-9._-]{1,128}$'
}

function Write-AdminJson([string]$path, $value) {
    $parent = Split-Path -Parent $path
    New-Item -ItemType Directory -Force -Path $parent | Out-Null
    $temporary = Join-Path $parent (".$([Guid]::NewGuid().ToString('N')).partial")
    $json = $value | ConvertTo-Json -Depth 10 -Compress
    [IO.File]::WriteAllText($temporary, $json, [Text.UTF8Encoding]::new($false))
    try {
        if (Test-Path -LiteralPath $path -PathType Leaf) {
            [IO.File]::Replace($temporary, $path, $null, $true)
        } else {
            [IO.File]::Move($temporary, $path)
        }
    } finally {
        Remove-Item -LiteralPath $temporary -Force -ErrorAction SilentlyContinue
    }
}

function Get-TextDigest([string]$value) {
    $algorithm = [Security.Cryptography.SHA256]::Create()
    try {
        $bytes = [Text.Encoding]::UTF8.GetBytes($value.ToLowerInvariant())
        return ([BitConverter]::ToString($algorithm.ComputeHash($bytes))).Replace('-', '').ToLowerInvariant()
    } finally { $algorithm.Dispose() }
}

function Require-UploadAuth($context, [string]$packageId, [string]$kind, [UInt64]$length, [string]$digest) {
    $timestamp = [string]$context.Request.Headers['X-qOS-Timestamp']
    $nonce = [string]$context.Request.Headers['X-qOS-Nonce']
    $provided = [string]$context.Request.Headers['X-qOS-Auth']
    $now = [DateTimeOffset]::UtcNow.ToUnixTimeSeconds(); $then = 0L
    if (-not [Int64]::TryParse($timestamp, [ref]$then) -or [Math]::Abs($now - $then) -gt 60) { throw 'stale installer upload' }
    if (-not (Test-OpaqueId $nonce 16) -or -not (Test-OpaqueId $packageId 16)) { throw 'invalid installer upload identity' }
    if ($kind -notin @('exe', 'msi') -or $digest -notmatch '^[A-Fa-f0-9]{64}$') { throw 'invalid installer upload metadata' }
    $canonical = "$timestamp`n$nonce`nUPLOAD`n$packageId`n$kind`n$length`n$($digest.ToLowerInvariant())`n"
    if (-not (Test-FixedTime (Get-Hmac $canonical) $provided)) { throw 'invalid installer upload authentication' }
    if (-not $seenNonces.TryAdd($nonce, $now)) { throw 'replayed bridge nonce' }
    foreach ($old in $seenNonces.GetEnumerator()) {
        if ($old.Value -lt ($now - 61)) { $discard = 0L; [void]$seenNonces.TryRemove($old.Key, [ref]$discard) }
    }
}

function Remove-OperationResources($operation) {
    if (-not [string]::IsNullOrWhiteSpace([string]$operation.taskName)) {
        Unregister-ScheduledTask -TaskName ([string]$operation.taskName) -Confirm:$false -ErrorAction SilentlyContinue
    }
    if (Test-OpaqueId ([string]$operation.operationId) 16) {
        Remove-Item -LiteralPath (Join-Path (Join-Path $installerRoot 'operations') ([string]$operation.operationId)) -Recurse -Force -ErrorAction SilentlyContinue
    }
    if (Test-OpaqueId ([string]$operation.packageId) 16) {
        $metadataPath = Join-Path $packageStateRoot "$($operation.packageId).json"
        if (Test-Path -LiteralPath $metadataPath -PathType Leaf) {
            try {
                $package = Get-Content -Raw -LiteralPath $metadataPath | ConvertFrom-Json
                $payload = [IO.Path]::GetFullPath([string]$package.payloadPath)
                if ($payload.StartsWith($installerRoot, [StringComparison]::OrdinalIgnoreCase)) {
                    Remove-Item -LiteralPath $payload -Force -ErrorAction SilentlyContinue
                }
            } catch { }
            Remove-Item -LiteralPath $metadataPath -Force -ErrorAction SilentlyContinue
        }
    }
}

function Reset-UninstallRecord($operation) {
    if ([string]$operation.operationKind -ne 'uninstallation' -or -not (Test-OpaqueId ([string]$operation.applicationId)) -or -not (Test-OpaqueId ([string]$operation.uninstallerId))) { return }
    $store = Get-UninstallRecordStore
    $record = @($store.records | Where-Object { $_.applicationId -eq [string]$operation.applicationId -and $_.id -eq [string]$operation.uninstallerId }) | Select-Object -First 1
    if ($null -eq $record) { return }
    $record.consumed = $false
    Write-AdminJson $uninstallRecordsPath @{ schemaVersion = 1; records = @($store.records | Where-Object { $_.applicationId -ne [string]$operation.applicationId }) + @($record) }
}

function Invoke-InstallerCleanup() {
    $now = [DateTimeOffset]::UtcNow.ToUnixTimeSeconds(); $visited = 0
    foreach ($metadataPath in @(Get-ChildItem -LiteralPath $packageStateRoot -Filter '*.json' -File -ErrorAction SilentlyContinue)) {
        if (++$visited -gt 256) { break }
        try {
            $package = Get-Content -Raw -LiteralPath $metadataPath.FullName | ConvertFrom-Json
            if ((-not [bool]$package.consumed -and [Int64]$package.expiresAtUnix -le $now) -or ([bool]$package.consumed -and [Int64]$package.expiresAtUnix -lt ($now - 86400))) {
                $payload = [IO.Path]::GetFullPath([string]$package.payloadPath)
                if ($payload.StartsWith($installerRoot, [StringComparison]::OrdinalIgnoreCase)) { Remove-Item -LiteralPath $payload -Force -ErrorAction SilentlyContinue }
                Remove-Item -LiteralPath $metadataPath.FullName -Force -ErrorAction SilentlyContinue
            }
        } catch { }
    }
    $visited = 0
    foreach ($operationPath in @(Get-ChildItem -LiteralPath $operationStateRoot -Filter '*.json' -File -ErrorAction SilentlyContinue)) {
        if (++$visited -gt 256) { break }
        try {
            $operation = Get-Content -Raw -LiteralPath $operationPath.FullName | ConvertFrom-Json
            if ([string]$operation.state -in @('completed', 'failed') -and [Int64]$operation.startedAtUnix -lt ($now - 604800)) {
                Remove-Item -LiteralPath $operationPath.FullName -Force -ErrorAction SilentlyContinue
            } elseif ([Int64]$operation.startedAtUnix -lt ($now - 86400)) {
                $operation.state = 'failed'; $operation.exitCode = $null; $operation.candidates = @()
                Write-AdminJson $operationPath.FullName $operation
                Reset-UninstallRecord $operation
                Remove-OperationResources $operation
            }
        } catch { }
    }
}

function Assert-StagingCapacity([UInt64]$incoming) {
    $total = [UInt64]0; $count = 0
    foreach ($metadataPath in @(Get-ChildItem -LiteralPath $packageStateRoot -Filter '*.json' -File -ErrorAction SilentlyContinue)) {
        $count++
        if ($count -gt 64) { throw 'too many staged installer packages' }
        try {
            $package = Get-Content -Raw -LiteralPath $metadataPath.FullName | ConvertFrom-Json
            if (-not [bool]$package.consumed) { $total += [UInt64]$package.byteLen }
        } catch { throw 'installer staging metadata is invalid' }
    }
    if ($total -ge $maximumStagedBytes -or $incoming -gt ($maximumStagedBytes - $total)) { throw 'installer staging capacity is exhausted' }
}

function Receive-Installer($context, [string]$packageId) {
    Invoke-InstallerCleanup
    $lengthSigned = [Int64]$context.Request.ContentLength64
    if ($lengthSigned -le 0) { throw 'installer upload needs an exact content length' }
    $length = [UInt64]$lengthSigned
    if ($length -gt $maximumInstallerBytes) { throw 'installer upload exceeds the appliance bound' }
    Assert-StagingCapacity $length
    $kind = [string]$context.Request.Headers['X-qOS-Installer-Kind']
    $expectedDigest = ([string]$context.Request.Headers['X-qOS-Payload-SHA256']).ToLowerInvariant()
    Require-UploadAuth $context $packageId $kind $length $expectedDigest
    $staging = Join-Path $installerRoot 'staging'
    $payloads = Join-Path $installerRoot 'payloads'
    $partial = Join-Path $staging "$packageId.partial"
    $final = Join-Path $payloads "$packageId.$kind"
    $metadataPath = Join-Path $packageStateRoot "$packageId.json"
    if ((Test-Path -LiteralPath $partial) -or (Test-Path -LiteralPath $final) -or (Test-Path -LiteralPath $metadataPath)) {
        throw 'installer package identity was already used'
    }
    $target = [IO.FileStream]::new($partial, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::None, 1048576, [IO.FileOptions]::WriteThrough)
    $hash = [Security.Cryptography.SHA256]::Create()
    $written = [UInt64]0
    try {
        $buffer = [byte[]]::new(1048576)
        while ($written -lt $length) {
            $remaining = $length - $written
            $wanted = [int][Math]::Min([UInt64]$buffer.Length, $remaining)
            $read = $context.Request.InputStream.Read($buffer, 0, $wanted)
            if ($read -le 0) { throw 'installer upload ended before its declared length' }
            $target.Write($buffer, 0, $read)
            [void]$hash.TransformBlock($buffer, 0, $read, $buffer, 0)
            $written += [UInt64]$read
        }
        if ($context.Request.InputStream.ReadByte() -ne -1) { throw 'installer upload exceeded its declared length' }
        [void]$hash.TransformFinalBlock([byte[]]::new(0), 0, 0)
        $actualDigest = ([BitConverter]::ToString($hash.Hash)).Replace('-', '').ToLowerInvariant()
        if ($written -ne $length -or -not (Test-FixedTime $actualDigest $expectedDigest)) { throw 'installer upload digest mismatch' }
        $target.Flush($true)
    } catch {
        $target.Dispose(); $hash.Dispose()
        Remove-Item -LiteralPath $partial -Force -ErrorAction SilentlyContinue
        throw
    }
    $target.Dispose(); $hash.Dispose()
    [IO.File]::Move($partial, $final)
    $expires = [DateTimeOffset]::UtcNow.AddMinutes(30).ToUnixTimeSeconds()
    Write-AdminJson $metadataPath @{ packageId = $packageId; kind = $kind; byteLen = $length; sha256 = $expectedDigest; expiresAtUnix = $expires; consumed = $false; payloadPath = $final }
    Write-Json $context @{ packageId = $packageId; byteLen = $length; sha256 = $expectedDigest; expiresAtUnix = $expires }
}

function Get-ProgramExecutables() {
    $roots = @($env:ProgramFiles, ${env:ProgramFiles(x86)}) | Where-Object { -not [string]::IsNullOrWhiteSpace($_) } | Select-Object -Unique
    $result = @(); $seen = 0
    foreach ($programRoot in $roots) {
        foreach ($item in @(Get-ChildItem -LiteralPath $programRoot -Filter '*.exe' -File -Recurse -ErrorAction SilentlyContinue)) {
            $seen++
            if ($seen -gt 50000) { throw 'managed Program Files executable inventory exceeded its bound' }
            $result += $item
        }
    }
    return @($result)
}

function ConvertTo-SafeOptionalMetadata($value) {
    if ($null -eq $value) { return $null }
    $text = [regex]::Replace([string]$value, '[\x00-\x1f\x7f]', ' ').Trim()
    if ([string]::IsNullOrWhiteSpace($text)) { return $null }
    return $text.Substring(0, [Math]::Min(256, $text.Length))
}

function Get-SafeUninstallProjection($record) {
    if ($null -eq $record) { return $null }
    return @{ id = [string]$record.id; displayName = [string]$record.displayName; displayVersion = $record.displayVersion; publisher = $record.publisher }
}

function Get-UninstallRecordStore() {
    if (-not (Test-Path -LiteralPath $uninstallRecordsPath -PathType Leaf)) {
        return @{ schemaVersion = 1; records = @() }
    }
    $store = Get-Content -Raw -LiteralPath $uninstallRecordsPath | ConvertFrom-Json
    if ([int]$store.schemaVersion -ne 1 -or $null -eq $store.records) { throw 'uninstall record store is invalid' }
    return @{ schemaVersion = 1; records = @($store.records) }
}

function Get-UninstallMetadata([string]$candidatePath, [string]$operationId) {
    $locations = @(
        'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\*',
        'HKLM:\SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall\*'
    )
    foreach ($entry in @(Get-ItemProperty -Path $locations -ErrorAction SilentlyContinue)) {
        $location = [string]$entry.InstallLocation
        $name = [string]$entry.DisplayName
        if ([string]::IsNullOrWhiteSpace($location) -or [string]::IsNullOrWhiteSpace($name)) { continue }
        try { $resolved = [IO.Path]::GetFullPath($location).TrimEnd('\') + '\' } catch { continue }
        if (-not $candidatePath.StartsWith($resolved, [StringComparison]::OrdinalIgnoreCase)) { continue }
        $productCode = [string]$entry.PSChildName
        if ($productCode -notmatch '^\{[A-Fa-f0-9]{8}-[A-Fa-f0-9]{4}-[A-Fa-f0-9]{4}-[A-Fa-f0-9]{4}-[A-Fa-f0-9]{12}\}$') { continue }
        $safeName = ConvertTo-SafeOptionalMetadata $name
        if ($null -eq $safeName) { continue }
        return @{ id = "uninstall-$((Get-TextDigest "$operationId|$($entry.PSPath)").Substring(0, 32))"; displayName = $safeName; displayVersion = (ConvertTo-SafeOptionalMetadata $entry.DisplayVersion); publisher = (ConvertTo-SafeOptionalMetadata $entry.Publisher); productCode = $productCode.ToUpperInvariant() }
    }
    return $null
}

function Get-DiscoveredCandidates($operation) {
    $before = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
    foreach ($digest in @($operation.beforeExecutableHashes)) { [void]$before.Add([string]$digest) }
    $candidates = @()
    foreach ($item in @(Get-ProgramExecutables)) {
        $path = [IO.Path]::GetFullPath($item.FullName)
        $pathDigest = Get-TextDigest $path
        if ($before.Contains($pathDigest) -and $item.LastWriteTimeUtc -lt ([DateTimeOffset]::FromUnixTimeSeconds([Int64]$operation.startedAtUnix).UtcDateTime.AddMinutes(-1))) { continue }
        $version = $item.VersionInfo
        $name = ConvertTo-SafeOptionalMetadata $version.FileDescription
        if ($null -eq $name) { $name = ConvertTo-SafeOptionalMetadata ([IO.Path]::GetFileNameWithoutExtension($path)) }
        if ($null -eq $name) { continue }
        $candidateId = "candidate-$((Get-TextDigest "$($operation.operationId)|$path").Substring(0, 32))"
        $candidates += @{ id = $candidateId; path = $path; displayName = $name; productName = (ConvertTo-SafeOptionalMetadata $version.ProductName); publisher = (ConvertTo-SafeOptionalMetadata $version.CompanyName); version = (ConvertTo-SafeOptionalMetadata $version.ProductVersion); uninstall = (Get-UninstallMetadata $path $operation.operationId) }
        if ($candidates.Count -ge 512) { break }
    }
    return @($candidates | Sort-Object displayName, id)
}

function Start-InstallerOperation($request) {
    Invoke-InstallerCleanup
    if (-not (Test-OpaqueId ([string]$request.packageId) 16) -or -not (Test-OpaqueId ([string]$request.operationId) 16)) { throw 'invalid installation operation identity' }
    $metadataPath = Join-Path $packageStateRoot "$($request.packageId).json"
    $operationPath = Join-Path $operationStateRoot "$($request.operationId).json"
    if (-not (Test-Path -LiteralPath $metadataPath -PathType Leaf) -or (Test-Path -LiteralPath $operationPath)) { throw 'installer package is unavailable' }
    $package = Get-Content -Raw -LiteralPath $metadataPath | ConvertFrom-Json
    $now = [DateTimeOffset]::UtcNow.ToUnixTimeSeconds()
    if ([bool]$package.consumed -or [Int64]$package.expiresAtUnix -le $now) { throw 'installer package is consumed or expired' }
    $payload = [IO.Path]::GetFullPath([string]$package.payloadPath)
    if ($payload -notmatch '^C:\\ProgramData\\qOS\\InstallerSession\\payloads\\[A-Za-z0-9._-]{1,128}\.(exe|msi)$' -or -not (Test-Path -LiteralPath $payload -PathType Leaf)) { throw 'installer payload is invalid' }
    $item = Get-Item -LiteralPath $payload
    $actualDigest = (Get-FileHash -LiteralPath $payload -Algorithm SHA256).Hash.ToLowerInvariant()
    if ([UInt64]$item.Length -ne [UInt64]$package.byteLen -or -not (Test-FixedTime $actualDigest ([string]$package.sha256))) { throw 'staged installer changed before use' }
    $user = Get-ManagedInteractiveUser
    if ($null -eq $user) { throw 'no managed interactive session exists for installation' }
    $before = @((Get-ProgramExecutables) | ForEach-Object { Get-TextDigest ([IO.Path]::GetFullPath($_.FullName)) })
    $account = New-Object Security.Principal.NTAccount($user)
    # Grant only directory traversal on the two parents. The selected payload,
    # helper and job live in an operation-specific directory with read/execute;
    # other staged or future packages remain SYSTEM/admin-only.
    foreach ($traversePath in @($installerRoot, (Join-Path $installerRoot 'operations'))) {
        $traverseAcl = Get-Acl -LiteralPath $traversePath
        $traverseRule = New-Object Security.AccessControl.FileSystemAccessRule($account, 'Traverse', 'None', 'None', 'Allow')
        [void]$traverseAcl.AddAccessRule($traverseRule)
        Set-Acl -LiteralPath $traversePath -AclObject $traverseAcl
    }
    $operationDirectory = Join-Path (Join-Path $installerRoot 'operations') ([string]$request.operationId)
    New-Item -ItemType Directory -Path $operationDirectory | Out-Null
    $operationAcl = New-Object Security.AccessControl.DirectorySecurity
    $operationAcl.SetAccessRuleProtection($true, $false)
    foreach ($principalName in @('SYSTEM', 'BUILTIN\Administrators')) {
        $fullRule = New-Object Security.AccessControl.FileSystemAccessRule($principalName, 'FullControl', 'ContainerInherit,ObjectInherit', 'None', 'Allow')
        [void]$operationAcl.AddAccessRule($fullRule)
    }
    $readRule = New-Object Security.AccessControl.FileSystemAccessRule($account, 'ReadAndExecute', 'ContainerInherit,ObjectInherit', 'None', 'Allow')
    [void]$operationAcl.AddAccessRule($readRule)
    Set-Acl -LiteralPath $operationDirectory -AclObject $operationAcl
    $operationPayload = Join-Path $operationDirectory "package.$($package.kind)"
    [IO.File]::Move($payload, $operationPayload)
    $package.payloadPath = $operationPayload
    $package.consumed = $true
    Write-AdminJson $metadataPath $package
    $taskName = "qOS-Install-$(([string]$request.operationId).Substring(0, 16))"
    $jobPath = Join-Path $operationDirectory 'job.json'
    Write-AdminJson $jobPath @{ operationId = [string]$request.operationId; packagePath = $operationPayload; kind = [string]$package.kind; byteLen = [UInt64]$package.byteLen; sha256 = [string]$package.sha256 }
    $helper = Join-Path $operationDirectory 'Start-qOSInstaller.ps1'
    Copy-Item -LiteralPath (Join-Path $installerRoot 'Start-qOSInstaller.ps1') -Destination $helper
    $operation = @{ operationKind = 'installation'; operationId = [string]$request.operationId; packageId = [string]$request.packageId; taskName = $taskName; startedAtUnix = $now; state = 'starting'; exitCode = $null; beforeExecutableHashes = $before; candidates = $null }
    Write-AdminJson $operationPath $operation
    $action = New-ScheduledTaskAction -Execute 'powershell.exe' -Argument "-NoLogo -NoProfile -NonInteractive -ExecutionPolicy RemoteSigned -File `"$helper`" -JobPath `"$jobPath`""
    $principal = New-ScheduledTaskPrincipal -UserId $user -LogonType InteractiveToken -RunLevel Limited
    $settings = New-ScheduledTaskSettingsSet -ExecutionTimeLimit (New-TimeSpan -Hours 12) -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries
    try {
        Register-ScheduledTask -TaskName $taskName -Action $action -Principal $principal -Settings $settings -Force | Out-Null
        Start-ScheduledTask -TaskName $taskName
    } catch {
        Unregister-ScheduledTask -TaskName $taskName -Confirm:$false -ErrorAction SilentlyContinue
        $operation.state = 'failed'; $operation.exitCode = $null; $operation.candidates = @()
        Write-AdminJson $operationPath $operation
        Remove-OperationResources $operation
        throw
    }
    return @{
        packageId = [string]$request.packageId
        operationId = [string]$request.operationId
        endpoint = "$guestIp`:3389"
        remoteAppAlias = [string]$request.operationId
        launchNonce = [Guid]::NewGuid().ToString('N')
        certificateSha256 = $certificateSha256
        presentationBackend = 'app_focused_spice'
    }
}

function Get-InstallationStatus([string]$operationId) {
    Invoke-InstallerCleanup
    if (-not (Test-OpaqueId $operationId 16)) { throw 'invalid installation operation identity' }
    $path = Join-Path $operationStateRoot "$operationId.json"
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) { throw 'installation operation is unknown' }
    $operation = Get-Content -Raw -LiteralPath $path | ConvertFrom-Json
    if ([string]$operation.operationKind -ne 'installation') { throw 'operation is not an installation' }
    if ([string]$operation.state -in @('completed', 'failed')) {
        $terminalCandidates = if ([string]$operation.state -eq 'completed') { @($operation.candidates) } else { @() }
        $safeTerminal = @($terminalCandidates | ForEach-Object { @{ id = [string]$_.id; displayName = [string]$_.displayName; productName = $_.productName; publisher = $_.publisher; version = $_.version; uninstall = (Get-SafeUninstallProjection $_.uninstall) } })
        return @{ operationId = $operationId; state = [string]$operation.state; exitCode = $operation.exitCode; candidates = $safeTerminal }
    }
    $task = Get-ScheduledTask -TaskName ([string]$operation.taskName) -ErrorAction SilentlyContinue
    if ($null -eq $task) {
        $operation.state = 'failed'; $operation.exitCode = $null; $operation.candidates = @(); Write-AdminJson $path $operation; Remove-OperationResources $operation
        return @{ operationId = $operationId; state = 'failed'; exitCode = $null; candidates = @() }
    }
    $info = Get-ScheduledTaskInfo -TaskName ([string]$operation.taskName)
    if ([string]$task.State -eq 'Running') { return @{ operationId = $operationId; state = 'running'; exitCode = $null; candidates = @() } }
    if ($info.LastRunTime.Year -lt 2000) { return @{ operationId = $operationId; state = 'starting'; exitCode = $null; candidates = @() } }
    $exitCode = [UInt32]$info.LastTaskResult
    if ($exitCode -notin @([UInt32]0, [UInt32]3010)) {
        $operation.state = 'failed'; $operation.exitCode = $exitCode; $operation.candidates = @(); Write-AdminJson $path $operation; Remove-OperationResources $operation
        return @{ operationId = $operationId; state = 'failed'; exitCode = $exitCode; candidates = @() }
    }
    $operation.state = 'completed'; $operation.exitCode = $exitCode
    $operation.candidates = @(Get-DiscoveredCandidates $operation)
    Write-AdminJson $path $operation
    Remove-OperationResources $operation
    $safeCandidates = @($operation.candidates | ForEach-Object { @{ id = [string]$_.id; displayName = [string]$_.displayName; productName = $_.productName; publisher = $_.publisher; version = $_.version; uninstall = (Get-SafeUninstallProjection $_.uninstall) } })
    return @{ operationId = $operationId; state = 'completed'; exitCode = $exitCode; candidates = $safeCandidates }
}

function Set-RemoteAppRegistry($app) {
    $base = 'HKLM:\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Terminal Server\TSAppAllowList\Applications'
    $key = Join-Path $base ([string]$app.id)
    New-Item -Path $key -Force | Out-Null
    New-ItemProperty -Path $key -Name Name -PropertyType String -Value ([string]$app.name) -Force | Out-Null
    New-ItemProperty -Path $key -Name Path -PropertyType String -Value ([string]$app.path) -Force | Out-Null
    New-ItemProperty -Path $key -Name CommandLine -PropertyType String -Value '' -Force | Out-Null
    New-ItemProperty -Path $key -Name ShowInTSWA -PropertyType DWord -Value 0 -Force | Out-Null
}

function Publish-DiscoveredCandidate($request) {
    foreach ($id in @($request.operationId, $request.candidateId, $request.applicationId)) { if (-not (Test-OpaqueId ([string]$id)) { throw 'invalid publication identity' } }
    $name = ([string]$request.displayName).Trim()
    if ([string]::IsNullOrWhiteSpace($name) -or $name.Length -gt 256 -or $name -match '[\x00-\x1f\x7f]') { throw 'invalid publication display name' }
    $status = Get-InstallationStatus ([string]$request.operationId)
    if ($status.state -ne 'completed') { throw 'installation has not completed' }
    $operation = Get-Content -Raw -LiteralPath (Join-Path $operationStateRoot "$($request.operationId).json") | ConvertFrom-Json
    $candidate = @($operation.candidates | Where-Object { $_.id -eq [string]$request.candidateId }) | Select-Object -First 1
    if ($null -eq $candidate) { throw 'candidate was not discovered for this operation' }
    $path = [IO.Path]::GetFullPath([string]$candidate.path)
    if ($path -notmatch '^(C:\\Program Files( \(x86\))?\\)' -or -not (Test-Path -LiteralPath $path -PathType Leaf)) { throw 'candidate executable is no longer managed' }
    $manifest = Get-PublishedManifest
    if ([UInt64]$manifest.generation -ne [UInt64]$request.expectedGeneration -or [UInt64]$manifest.generation -eq [UInt64]::MaxValue) { throw 'publication generation changed' }
    if (@($manifest.applications | Where-Object { $_.id -eq [string]$request.applicationId }).Count -ne 0) { throw 'application identity is already published' }
    $app = @{ id = [string]$request.applicationId; name = $name; path = $path; arguments = '' }
    Set-RemoteAppRegistry $app
    $uninstallStoreBefore = Get-UninstallRecordStore
    $uninstallStoreChanged = $false
    if ($null -ne $candidate.uninstall) {
        $productCode = [string]$candidate.uninstall.productCode
        if (-not (Test-OpaqueId ([string]$candidate.uninstall.id)) -or $productCode -notmatch '^\{[A-Fa-f0-9]{8}-[A-Fa-f0-9]{4}-[A-Fa-f0-9]{4}-[A-Fa-f0-9]{4}-[A-Fa-f0-9]{12}\}$') { throw 'discovered uninstaller record is invalid' }
        $record = @{ applicationId = [string]$request.applicationId; id = [string]$candidate.uninstall.id; displayName = [string]$candidate.uninstall.displayName; displayVersion = $candidate.uninstall.displayVersion; publisher = $candidate.uninstall.publisher; productCode = $productCode.ToUpperInvariant(); consumed = $false }
        Write-AdminJson $uninstallRecordsPath @{ schemaVersion = 1; records = @($uninstallStoreBefore.records | Where-Object { $_.applicationId -ne [string]$request.applicationId }) + @($record) }
        $uninstallStoreChanged = $true
    }
    try {
        Write-AdminJson $appsPath @{ generation = ([UInt64]$manifest.generation + 1); applications = @($manifest.applications) + @($app) }
    } catch {
        if ($uninstallStoreChanged) { Write-AdminJson $uninstallRecordsPath $uninstallStoreBefore }
        Remove-Item -LiteralPath ("HKLM:\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Terminal Server\TSAppAllowList\Applications\$($request.applicationId)") -Recurse -Force -ErrorAction SilentlyContinue
        throw
    }
    return @{ applicationId = [string]$request.applicationId; catalogGeneration = ([UInt64]$manifest.generation + 1) }
}

function Remove-Publication($request) {
    if (-not (Test-OpaqueId ([string]$request.applicationId))) { throw 'invalid publication identity' }
    $manifest = Get-PublishedManifest
    if ([UInt64]$manifest.generation -ne [UInt64]$request.expectedGeneration -or [UInt64]$manifest.generation -eq [UInt64]::MaxValue) { throw 'publication generation changed' }
    $remaining = @($manifest.applications | Where-Object { $_.id -ne [string]$request.applicationId })
    if ($remaining.Count -eq @($manifest.applications).Count) { throw 'application is not published' }
    $next = [UInt64]$manifest.generation + 1
    Write-AdminJson $appsPath @{ generation = $next; applications = $remaining }
    Remove-Item -LiteralPath ("HKLM:\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Terminal Server\TSAppAllowList\Applications\$($request.applicationId)") -Recurse -Force -ErrorAction SilentlyContinue
    return @{ applicationId = [string]$request.applicationId; catalogGeneration = $next }
}

function Start-UninstallOperation($request) {
    Invoke-InstallerCleanup
    foreach ($id in @($request.applicationId, $request.operationId)) { if (-not (Test-OpaqueId ([string]$id) 16)) { throw 'invalid uninstall operation identity' } }
    $operationPath = Join-Path $operationStateRoot "$($request.operationId).json"
    if (Test-Path -LiteralPath $operationPath) { throw 'uninstall operation identity was already used' }
    $store = Get-UninstallRecordStore
    $record = @($store.records | Where-Object { $_.applicationId -eq [string]$request.applicationId }) | Select-Object -First 1
    if ($null -eq $record -or [bool]$record.consumed -or [string]$record.productCode -notmatch '^\{[A-Fa-f0-9]{8}-[A-Fa-f0-9]{4}-[A-Fa-f0-9]{4}-[A-Fa-f0-9]{4}-[A-Fa-f0-9]{12}\}$') { throw 'safe discovered uninstaller is unavailable' }
    $user = Get-ManagedInteractiveUser
    if ($null -eq $user) { throw 'no managed interactive session exists for uninstallation' }
    $account = New-Object Security.Principal.NTAccount($user)
    foreach ($traversePath in @($installerRoot, (Join-Path $installerRoot 'operations'))) {
        $traverseAcl = Get-Acl -LiteralPath $traversePath
        $traverseRule = New-Object Security.AccessControl.FileSystemAccessRule($account, 'Traverse', 'None', 'None', 'Allow')
        [void]$traverseAcl.AddAccessRule($traverseRule); Set-Acl -LiteralPath $traversePath -AclObject $traverseAcl
    }
    $operationDirectory = Join-Path (Join-Path $installerRoot 'operations') ([string]$request.operationId)
    New-Item -ItemType Directory -Path $operationDirectory | Out-Null
    $operationAcl = New-Object Security.AccessControl.DirectorySecurity
    $operationAcl.SetAccessRuleProtection($true, $false)
    foreach ($principalName in @('SYSTEM', 'BUILTIN\Administrators')) {
        $fullRule = New-Object Security.AccessControl.FileSystemAccessRule($principalName, 'FullControl', 'ContainerInherit,ObjectInherit', 'None', 'Allow')
        [void]$operationAcl.AddAccessRule($fullRule)
    }
    $readRule = New-Object Security.AccessControl.FileSystemAccessRule($account, 'ReadAndExecute', 'ContainerInherit,ObjectInherit', 'None', 'Allow')
    [void]$operationAcl.AddAccessRule($readRule); Set-Acl -LiteralPath $operationDirectory -AclObject $operationAcl
    $jobPath = Join-Path $operationDirectory 'job.json'
    Write-AdminJson $jobPath @{ operationId = [string]$request.operationId; productCode = [string]$record.productCode }
    $helper = Join-Path $operationDirectory 'Start-qOSUninstaller.ps1'
    Copy-Item -LiteralPath (Join-Path $installerRoot 'Start-qOSUninstaller.ps1') -Destination $helper
    $taskName = "qOS-Uninstall-$(([string]$request.operationId).Substring(0, 16))"
    $now = [DateTimeOffset]::UtcNow.ToUnixTimeSeconds()
    $operation = @{ operationKind = 'uninstallation'; operationId = [string]$request.operationId; applicationId = [string]$request.applicationId; uninstallerId = [string]$record.id; taskName = $taskName; startedAtUnix = $now; state = 'starting'; exitCode = $null; catalogGeneration = $null }
    Write-AdminJson $operationPath $operation
    $record.consumed = $true
    Write-AdminJson $uninstallRecordsPath @{ schemaVersion = 1; records = @($store.records | Where-Object { $_.applicationId -ne [string]$request.applicationId }) + @($record) }
    $action = New-ScheduledTaskAction -Execute 'powershell.exe' -Argument "-NoLogo -NoProfile -NonInteractive -ExecutionPolicy RemoteSigned -File `"$helper`" -JobPath `"$jobPath`""
    $principal = New-ScheduledTaskPrincipal -UserId $user -LogonType InteractiveToken -RunLevel Limited
    $settings = New-ScheduledTaskSettingsSet -ExecutionTimeLimit (New-TimeSpan -Hours 12) -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries
    try {
        Register-ScheduledTask -TaskName $taskName -Action $action -Principal $principal -Settings $settings -Force | Out-Null
        Start-ScheduledTask -TaskName $taskName
    } catch {
        Write-AdminJson $uninstallRecordsPath $store
        $operation.state = 'failed'; Write-AdminJson $operationPath $operation; Remove-OperationResources $operation
        throw
    }
    return @{ applicationId = [string]$request.applicationId; operationId = [string]$request.operationId; endpoint = "$guestIp`:3389"; remoteAppAlias = [string]$request.operationId; launchNonce = [Guid]::NewGuid().ToString('N'); certificateSha256 = $certificateSha256; presentationBackend = 'app_focused_spice' }
}

function Complete-UninstallPublication($operation) {
    $manifest = Get-PublishedManifest
    $remaining = @($manifest.applications | Where-Object { $_.id -ne [string]$operation.applicationId })
    $generation = [UInt64]$manifest.generation
    if ($remaining.Count -ne @($manifest.applications).Count) {
        if ($generation -eq [UInt64]::MaxValue) { throw 'publication generation is exhausted' }
        $generation++
        Write-AdminJson $appsPath @{ generation = $generation; applications = $remaining }
    }
    Remove-Item -LiteralPath ("HKLM:\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Terminal Server\TSAppAllowList\Applications\$($operation.applicationId)") -Recurse -Force -ErrorAction SilentlyContinue
    $store = Get-UninstallRecordStore
    Write-AdminJson $uninstallRecordsPath @{ schemaVersion = 1; records = @($store.records | Where-Object { $_.applicationId -ne [string]$operation.applicationId }) }
    return $generation
}

function Get-UninstallationStatus([string]$operationId) {
    Invoke-InstallerCleanup
    if (-not (Test-OpaqueId $operationId 16)) { throw 'invalid uninstall operation identity' }
    $path = Join-Path $operationStateRoot "$operationId.json"
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) { throw 'uninstall operation is unknown' }
    $operation = Get-Content -Raw -LiteralPath $path | ConvertFrom-Json
    if ([string]$operation.operationKind -ne 'uninstallation') { throw 'operation is not an uninstallation' }
    if ([string]$operation.state -in @('completed', 'failed')) {
        return @{ operationId = $operationId; state = [string]$operation.state; exitCode = $operation.exitCode; catalogGeneration = $operation.catalogGeneration }
    }
    $task = Get-ScheduledTask -TaskName ([string]$operation.taskName) -ErrorAction SilentlyContinue
    if ($null -eq $task) {
        $operation.state = 'failed'; $operation.exitCode = $null; Write-AdminJson $path $operation; Reset-UninstallRecord $operation; Remove-OperationResources $operation
        return @{ operationId = $operationId; state = 'failed'; exitCode = $null; catalogGeneration = $null }
    }
    $info = Get-ScheduledTaskInfo -TaskName ([string]$operation.taskName)
    if ([string]$task.State -eq 'Running') { return @{ operationId = $operationId; state = 'running'; exitCode = $null; catalogGeneration = $null } }
    if ($info.LastRunTime.Year -lt 2000) { return @{ operationId = $operationId; state = 'starting'; exitCode = $null; catalogGeneration = $null } }
    $exitCode = [UInt32]$info.LastTaskResult
    if ($exitCode -notin @([UInt32]0, [UInt32]3010)) {
        $operation.state = 'failed'; $operation.exitCode = $exitCode; Write-AdminJson $path $operation; Reset-UninstallRecord $operation; Remove-OperationResources $operation
        return @{ operationId = $operationId; state = 'failed'; exitCode = $exitCode; catalogGeneration = $null }
    }
    $operation.catalogGeneration = (Complete-UninstallPublication $operation)
    $operation.state = 'completed'; $operation.exitCode = $exitCode
    Write-AdminJson $path $operation; Remove-OperationResources $operation
    return @{ operationId = $operationId; state = 'completed'; exitCode = $exitCode; catalogGeneration = [UInt64]$operation.catalogGeneration }
}

# Generate a VM-local TLS identity once. The certificate has no host trust
# anchor; qOS learns and pins its SHA-256 thumbprint through authenticated
# enrollment, then refuses certificate changes until explicit repair.
$cert = Get-ChildItem Cert:\LocalMachine\My | Where-Object { $_.FriendlyName -eq 'qOS guest bridge' } | Select-Object -First 1
if ($null -eq $cert) {
    $cert = New-SelfSignedCertificate -DnsName 'qos-windows-appliance' -CertStoreLocation Cert:\LocalMachine\My -FriendlyName 'qOS guest bridge' -KeyAlgorithm RSA -KeyLength 3072 -HashAlgorithm SHA256 -NotAfter (Get-Date).AddYears(5)
}
$certificateSha256 = [Convert]::ToHexString([Security.Cryptography.SHA256]::HashData($cert.RawData)).ToLowerInvariant()
& netsh http delete sslcert ipport="$guestIp`:$port" 2>$null | Out-Null
& netsh http add sslcert ipport="$guestIp`:$port" certhash=$($cert.Thumbprint) appid='{66D23091-6E22-4A18-B2F3-2A9318A2D9E1}' certstorename=MY | Out-Null

$listener = [Net.HttpListener]::new()
$listener.Prefixes.Add("https://$guestIp`:$port/")
$null = New-Item -ItemType Directory -Force -Path $packageStateRoot
$null = New-Item -ItemType Directory -Force -Path $operationStateRoot
$listener.Start()
while ($listener.IsListening) {
    $context = $listener.GetContext()
    try {
        $requestPath = [string]$context.Request.Url.AbsolutePath
        if ($context.Request.HttpMethod -eq 'POST' -and $requestPath -match '^/v1/installers/([A-Za-z0-9._-]{16,128})$') {
            Receive-Installer $context $Matches[1]
            continue
        }
        $reader = [IO.StreamReader]::new($context.Request.InputStream, $context.Request.ContentEncoding)
        $body = $reader.ReadToEnd(); $reader.Dispose()
        Require-RequestAuth $context $body
        if ($context.Request.HttpMethod -eq 'GET' -and $requestPath -match '^/v1/installations/([A-Za-z0-9._-]{16,128})$') {
            Write-Json $context (Get-InstallationStatus $Matches[1])
            continue
        }
        if ($context.Request.HttpMethod -eq 'GET' -and $requestPath -match '^/v1/uninstallations/([A-Za-z0-9._-]{16,128})$') {
            Write-Json $context (Get-UninstallationStatus $Matches[1])
            continue
        }
        switch ("$($context.Request.HttpMethod) $($context.Request.Url.AbsolutePath)") {
            'GET /v1/health' {
                $windows = Get-CimInstance -ClassName SoftwareLicensingProduct | Where-Object { $_.ApplicationID -eq '55c92734-d682-4d71-983e-d6ec3f16059f' -and $_.PartialProductKey } | Select-Object -First 1
                $license = if ($null -ne $windows -and $windows.LicenseStatus -eq 1) { 'licensed' } else { 'unlicensed' }
                $os = Get-CimInstance -ClassName Win32_OperatingSystem
                $presentation = if ($os.ProductType -eq 3) { 'native_rail_rds' } else { 'native_rail_client_best_effort' }
                $interactiveUser = Get-ManagedInteractiveUser
                $catalog = Get-PublishedCatalog
                $bootId = [string]$os.LastBootUpTime.ToUniversalTime().ToFileTimeUtc()
                $certificateDerBase64 = [Convert]::ToBase64String($cert.RawData)
                Write-Json $context @{ protocolMajor = 1; certificateSha256 = $certificateSha256; certificateDerBase64 = $certificateDerBase64; license = $license; catalogGeneration = $catalog.generation; guestBootId = $bootId; presentationBackend = $presentation; appFocusedSpiceAvailable = ($null -ne $interactiveUser); appFocusedSpiceUnavailableReason = if ($null -eq $interactiveUser) { 'no managed interactive session' } else { $null } }; break
            }
            'GET /v1/catalog' { Write-Json $context (Get-PublishedCatalog); break }
            'POST /v1/installations' {
                Write-Json $context (Start-InstallerOperation ($body | ConvertFrom-Json))
                break
            }
            'POST /v1/publications' {
                Write-Json $context (Publish-DiscoveredCandidate ($body | ConvertFrom-Json))
                break
            }
            'POST /v1/publications/remove' {
                Write-Json $context (Remove-Publication ($body | ConvertFrom-Json))
                break
            }
            'POST /v1/uninstallations' {
                Write-Json $context (Start-UninstallOperation ($body | ConvertFrom-Json))
                break
            }
            'POST /v1/launch' {
                $request = $body | ConvertFrom-Json
                if ($request.applicationId -notmatch '^[A-Za-z0-9._-]{1,128}$' -or $request.launchNonce -notmatch '^[A-Za-z0-9-]{16,128}$') { throw 'invalid launch request' }
                $app = (Get-PublishedManifest).applications | Where-Object { $_.id -eq $request.applicationId } | Select-Object -First 1
                if ($null -eq $app) { throw 'application is not published' }
                $requestedPresentation = [string]$request.presentationBackend
                if ([string]::IsNullOrWhiteSpace($requestedPresentation)) { $requestedPresentation = 'native_rail' }
                if ($requestedPresentation -notin @('native_rail', 'native_rail_rds', 'native_rail_client_best_effort', 'app_focused_spice')) { throw 'unsupported presentation backend' }
                # Native RAIL needs a one-use account because it connects to
                # the guest RDP listener. Focused SPICE attaches to the
                # singleton VM display instead; creating an account there
                # would be unnecessary guest state and an accidental second
                # access path.
                $name = $null
                $password = $null
                if ($requestedPresentation -ne 'app_focused_spice') {
                    $name = "qra$([Guid]::NewGuid().ToString('N').Substring(0, 16))"
                    $passwordBytes = [byte[]]::new(32)
                    # .NET Framework (and therefore Windows PowerShell 5.1 on
                    # supported Windows Server releases) has Create/GetBytes but
                    # not the newer static RandomNumberGenerator.Fill API.
                    $rng = [Security.Cryptography.RandomNumberGenerator]::Create()
                    try { $rng.GetBytes($passwordBytes) }
                    finally { $rng.Dispose() }
                    $password = [Convert]::ToBase64String($passwordBytes) + '!qA1'
                    $secure = ConvertTo-SecureString $password -AsPlainText -Force
                    New-LocalUser -Name $name -Password $secure -AccountNeverExpires -PasswordNeverExpires | Out-Null
                    Add-LocalGroupMember -Group 'Remote Desktop Users' -Member $name
                    $when = (Get-Date).AddMinutes(10)
                    $taskName = "qOS-Remove-$name"
                    # ScheduledTasks takes a DateTime directly, avoiding
                    # schtasks.exe's locale-dependent /SD date syntax. Deleting
                    # the task as part of its own action prevents steady-state
                    # task-list growth after many RemoteApp sessions.
                    $cleanupCommand = "Remove-LocalUser -Name '$name' -ErrorAction SilentlyContinue; Unregister-ScheduledTask -TaskName '$taskName' -Confirm:`$false"
                    $cleanupAction = New-ScheduledTaskAction -Execute 'powershell.exe' -Argument "-NoLogo -NoProfile -NonInteractive -ExecutionPolicy RemoteSigned -Command `"$cleanupCommand`""
                    $cleanupTrigger = New-ScheduledTaskTrigger -Once -At $when
                    $cleanupPrincipal = New-ScheduledTaskPrincipal -UserId 'SYSTEM' -LogonType ServiceAccount -RunLevel Highest
                    Register-ScheduledTask -TaskName $taskName -Action $cleanupAction -Trigger $cleanupTrigger -Principal $cleanupPrincipal -Force | Out-Null
                }
                $os = Get-CimInstance -ClassName Win32_OperatingSystem
                $presentation = if ($os.ProductType -eq 3) { 'native_rail_rds' } else { 'native_rail_client_best_effort' }
                $focus = $null
                if ($requestedPresentation -eq 'app_focused_spice') { $focus = Start-FocusedApplication $app }
                Write-Json $context @{ endpoint = "$guestIp`:3389"; username = $name; password = $password; remoteAppAlias = $app.id; launchNonce = $request.launchNonce; certificateSha256 = $certificateSha256; presentationBackend = if ($requestedPresentation -eq 'app_focused_spice') { 'app_focused_spice' } else { $presentation } }
                break
            }
            default { Write-Json $context @{ error = 'not found' } 404; break }
        }
    } catch {
        if (-not $context.Response.OutputStream.CanWrite) { continue }
        Write-Json $context @{ error = 'request rejected' } 403
    }
}

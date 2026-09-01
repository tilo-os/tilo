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
    $published = @()
    foreach ($app in @($source.applications)) {
        $published += @{ id = [string]$app.id; displayName = [string]$app.name; remoteAppAlias = [string]$app.id; associations = @() }
    }
    return @{ generation = $source.generation; applications = $published }
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
$listener.Start()
while ($listener.IsListening) {
    $context = $listener.GetContext()
    try {
        $reader = [IO.StreamReader]::new($context.Request.InputStream, $context.Request.ContentEncoding)
        $body = $reader.ReadToEnd(); $reader.Dispose()
        Require-RequestAuth $context $body
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

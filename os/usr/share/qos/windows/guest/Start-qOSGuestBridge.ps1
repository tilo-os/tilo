<#
The guest endpoint is intentionally small: catalog, health and preparation of
one single-use RemoteApp credential. It is an HTTPS service bound only to the
non-forwarding qOS host-only network. Every request also needs a fresh HMAC
over timestamp, nonce and body using the bootstrap key installed SYSTEM-only.

The host pins the TLS certificate returned by the authenticated health call;
the bootstrap HMAC is an enrollment channel, not a replacement for TLS.
#>
[CmdletBinding()]
param()
$ErrorActionPreference = 'Stop'
$root = 'C:\ProgramData\qOS\RemoteApp'
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
                Write-Json $context @{ protocolMajor = 1; certificateSha256 = $certificateSha256; license = $license; catalogGeneration = 1 }; break
            }
            'GET /v1/catalog' { Write-Json $context (Get-Content -Raw -LiteralPath $appsPath | ConvertFrom-Json); break }
            'POST /v1/launch' {
                $request = $body | ConvertFrom-Json
                if ($request.applicationId -notmatch '^[A-Za-z0-9._-]{1,128}$' -or $request.launchNonce -notmatch '^[A-Za-z0-9-]{16,128}$') { throw 'invalid launch request' }
                $app = (Get-Content -Raw -LiteralPath $appsPath | ConvertFrom-Json).applications | Where-Object { $_.id -eq $request.applicationId } | Select-Object -First 1
                if ($null -eq $app) { throw 'application is not published' }
                # A unique short-lived account prevents password reuse between
                # launches. RDS RemoteApp, not a hidden desktop, authorises the
                # application surface. A scheduled deletion bounds a failed
                # FreeRDP launch too.
                $name = "qra$([Guid]::NewGuid().ToString('N').Substring(0, 16))"
                $passwordBytes = [byte[]]::new(32)
                [Security.Cryptography.RandomNumberGenerator]::Fill($passwordBytes)
                $password = [Convert]::ToBase64String($passwordBytes) + '!qA1'
                $secure = ConvertTo-SecureString $password -AsPlainText -Force
                New-LocalUser -Name $name -Password $secure -AccountNeverExpires -PasswordNeverExpires | Out-Null
                Add-LocalGroupMember -Group 'Remote Desktop Users' -Member $name
                $when = (Get-Date).AddMinutes(10)
                $taskName = "qOS-Remove-$name"
                $delete = "cmd.exe /d /c `"net user $name /delete & schtasks /Delete /TN $taskName /F`""
                schtasks.exe /Create /TN $taskName /SC ONCE /ST $($when.ToString('HH:mm')) /SD $($when.ToString('MM/dd/yyyy')) /RU SYSTEM /RL HIGHEST /TR $delete /F | Out-Null
                Write-Json $context @{ endpoint = "$guestIp`:3389"; bridgePort = $port; username = $name; password = $password; remoteAppAlias = $app.id; launchNonce = $request.launchNonce; certificateSha256 = $certificateSha256 }
                break
            }
            default { Write-Json $context @{ error = 'not found' } 404; break }
        }
    } catch {
        if (-not $context.Response.OutputStream.CanWrite) { continue }
        Write-Json $context @{ error = 'request rejected' } 403
    }
}

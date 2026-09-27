param([string]$Store = (Join-Path $env:LOCALAPPDATA 'Tilo\InstallSecrets'))

$ErrorActionPreference = 'Stop'
$user = [Security.Principal.WindowsIdentity]::GetCurrent().Name.Split('\')[-1]

function Read-ProtectedPassword([string]$name) {
    $path = Join-Path $Store $name
    $ciphertext = (Get-Content -LiteralPath $path -Raw).Trim()
    $secure = ConvertTo-SecureString -String $ciphertext
    $credential = [pscredential]::new($user, $secure)
    return $credential.GetNetworkCredential().Password
}

$account = Read-ProtectedPassword 'account-password.secure'
$disk = Read-ProtectedPassword 'disk-unlock.secure'
if (-not $account -or -not $disk -or $account -eq $disk) {
    throw 'Stored account and disk passwords must be distinct and nonempty'
}

@{ account_password = $account; disk_passphrase = $disk } |
    ConvertTo-Json -Compress

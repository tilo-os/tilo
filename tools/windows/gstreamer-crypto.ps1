# Match the repository's distinction between Tilo-owned crypto and reviewed
# distribution-library residuals. Do not widen this list for unrelated plugins.
function Assert-TiloMediaCryptoConsumer {
    param(
        [Parameter(Mandatory)][string]$Consumer,
        [Parameter(Mandatory)][string]$Dependency,
        [Parameter(Mandatory)][string]$Package,
        [Parameter(Mandatory)][string]$Version
    )
    $allowed = @{
        'libsrt.dll' = @('srt', '1.5.7-1')
        'libssh.dll' = @('libssh', '0.12.2-1')
    }
    if ($Dependency -notmatch '^libcrypto-3(?:-(?:arm64|x64))?\.dll$' -or
        -not $allowed.ContainsKey($Consumer)) {
        throw "Unreviewed OpenSSL import: $Consumer -> $Dependency ($Package $Version)."
    }
    $rule = $allowed[$Consumer]
    if ($Package -notmatch ('^mingw-w64-(?:clang-aarch64|ucrt-x86_64|x86_64)-' + [regex]::Escape($rule[0]) + '$') -or
        $Version -ne $rule[1]) {
        throw "Review the changed OpenSSL consumer before bundling: $Consumer ($Package $Version)."
    }
}

function Test-TiloMediaOpenSslImport([string]$Name) {
    return $Name -match '^(?:libssl|libcrypto|libeay32|ssleay32).*\.dll$'
}

function Assert-TiloMediaNoOptionalOpenSsl([string]$Directory) {
    if (-not (Test-Path -LiteralPath $Directory -PathType Container)) { return }
    $forbidden = @(Get-ChildItem -LiteralPath $Directory -Recurse -File | Where-Object {
        $_.Name -match '^(?:libssl|libeay32|ssleay32|libgioopenssl).*\.dll$'
    })
    if ($forbidden.Count) {
        throw "Runtime contains forbidden optional OpenSSL payloads; use a separately reviewed clean destination. Nothing was removed: $($forbidden.FullName -join ', ')"
    }
}

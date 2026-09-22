<# Run the receipt-driven local Tilo ARM64 VM development CLI. #>
[CmdletBinding(PositionalBinding = $false)]
param(
    [Parameter(ValueFromRemainingArguments = $true)]
    [string[]]$Arguments
)

$ErrorActionPreference = 'Stop'
$python = Get-Command python -ErrorAction SilentlyContinue
if (-not $python) {
    $python = Get-Command py -ErrorAction SilentlyContinue
}
if (-not $python) {
    throw 'Python 3 was not found in PATH.'
}

$cli = Join-Path $PSScriptRoot 'tilo_vm.py'
if ($python.Name -eq 'py.exe') {
    & $python.Source -3 $cli @Arguments
} else {
    & $python.Source $cli @Arguments
}
exit $LASTEXITCODE

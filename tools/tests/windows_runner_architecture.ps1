# Load only the pure architecture resolver. Never execute registration or read
# credentials from the ephemeral-runner launcher during this test.
$ErrorActionPreference = 'Stop'
$Tokens = $null
$Errors = $null
$Ast = [Management.Automation.Language.Parser]::ParseFile(
    (Join-Path $PSScriptRoot '../Run-ImagerEphemeralRunner.ps1'), [ref]$Tokens, [ref]$Errors)
if ($Errors.Count -ne 0) { throw 'Runner script contains syntax errors' }
$Function = $Ast.Find({ param($Node)
    $Node -is [Management.Automation.Language.FunctionDefinitionAst] -and
    $Node.Name -eq 'Resolve-TiloWindowsRunnerLabel'
}, $true)
if (-not $Function) { throw 'Missing native architecture resolver' }
. ([ScriptBlock]::Create($Function.Extent.Text))
if ((Resolve-TiloWindowsRunnerLabel @(9)) -ne 'imager-windows-x86_64') { throw 'x86-64 routing failed' }
if ((Resolve-TiloWindowsRunnerLabel @(12, 12)) -ne 'imager-windows-aarch64') { throw 'ARM64 routing failed' }
foreach ($Invalid in @(@(), @(0), @(9, 12))) {
    $Rejected = $false
    try { Resolve-TiloWindowsRunnerLabel $Invalid | Out-Null } catch { $Rejected = $true }
    if (-not $Rejected) { throw 'Ambiguous or unsupported hardware was accepted' }
}
Write-Output 'PASS: native Windows runner labels; missing, mixed and unsupported architectures rejected.'

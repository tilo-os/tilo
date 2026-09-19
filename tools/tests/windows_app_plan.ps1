$ErrorActionPreference = 'Stop'
$planner = Join-Path $PSScriptRoot '../plan-windows-apps.ps1'
foreach ($arch in @('x86_64', 'aarch64')) {
    $plan = & $planner -Apps calculator,notes,calculator -Architecture $arch | ConvertFrom-Json
    if ($plan.publicationEnabled -ne $false) { throw 'Planning enabled publication.' }
    if ($plan.runnerLabel -ne "imager-windows-$arch") { throw 'Wrong native runner label.' }
    if ($plan.nativeRustHost -ne "$arch-pc-windows-msvc") { throw 'Wrong compiler host.' }
    if (($plan.applications -join ',') -ne 'calculator,notes') { throw 'Selection is not deterministic.' }
    if (($plan.cargo.arguments -join ' ') -notmatch '--locked.*-p calculator -p notes') { throw 'Selected native build contract missing.' }
    if ($plan.cache.inputs.Count -lt 9) { throw 'Missing inspected cache inputs.' }
}
foreach ($app in @('nonexistent', '../calculator', 'Calculator', '')) {
    $rejected = $false
    try { & $planner -Apps @($app) -Architecture x86_64 | Out-Null } catch { $rejected = $true }
    if (-not $rejected) { throw "Unsupported installer selection accepted: $app" }
}
Write-Output 'PASS: native Windows planning follows the production installer allowlist without executing it.'

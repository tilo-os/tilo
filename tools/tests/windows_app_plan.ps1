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
    if ($plan.cache.reusable -ne $false) { throw 'Unqualified inventory advertised as a cache key.' }
    if ($plan.builds.Count -ne 1) { throw 'Ordinary apps scheduled an unnecessary helper build.' }
    if ($plan.runtime.pdfium -or $plan.runtime.notifications -or -not $plan.runtime.dxc -or
        $plan.runtime.fonts -ne 'crates/sdk/ui-core/assets/fonts' -or
        $plan.runtime.gstreamer -notmatch 'PE imports') { throw 'Ordinary runtime requirements are wrong.' }
    foreach ($app in @('clocks', 'pdf-viewer', 'file-explorer')) {
        $special = & $planner -Apps $app -Architecture $arch | ConvertFrom-Json
        if ($special.runtime.pdfium -ne ($app -ne 'clocks')) { throw "Wrong PDFium requirement: $app" }
        if ($special.runtime.notifications -ne ($app -eq 'clocks')) { throw "Wrong notification requirement: $app" }
        if ($app -eq 'clocks') {
            if ($special.builds.Count -ne 2 -or
                ($special.builds[1].arguments -join ' ') -ne
                'build --locked --profile release --jobs 2 --message-format=json-render-diagnostics -p ui-examples --example windows-host') {
                throw 'Clocks qualification omits its actual notification helper build.'
            }
        } elseif ($special.builds.Count -ne 1) { throw "Unexpected helper build for $app" }
    }
}
foreach ($app in @('nonexistent', '../calculator', 'Calculator', '')) {
    $rejected = $false
    try { & $planner -Apps @($app) -Architecture x86_64 | Out-Null } catch { $rejected = $true }
    if (-not $rejected) { throw "Unsupported installer selection accepted: $app" }
}

# Execute the production installer's build section with a fake Cargo boundary.
# Exclude its public-directory write probes and the subsequent installation, so
# this test never compiles, downloads or touches the user's installation.
$installer = Join-Path $PSScriptRoot '../windows/install-apps.ps1'
$source = Get-Content -LiteralPath $installer -Raw
$tokens = $null
$errors = $null
$ast = [Management.Automation.Language.Parser]::ParseFile($installer, [ref]$tokens, [ref]$errors)
if ($errors.Count) { throw 'Installer does not parse.' }
$start = @($ast.EndBlock.Statements | Where-Object {
    $_ -is [Management.Automation.Language.AssignmentStatementAst] -and $_.Left.Extent.Text -eq '$buildPlan'
})
$stop = @($ast.EndBlock.Statements | Where-Object {
    $_.Extent.Text -eq 'New-Item -ItemType Directory -Path $installDirectory -Force | Out-Null'
})
if ($start.Count -ne 1 -or $stop.Count -ne 1 -or $start[0].Extent.StartOffset -ge $stop[0].Extent.StartOffset) {
    throw 'Cannot isolate the production build section safely.'
}
if (-not $source.Contains(". (Join-Path `$PSScriptRoot 'app-build-plan.ps1')")) {
    throw 'Installer does not load the shared build plan.'
}
$productionBuild = [ScriptBlock]::Create($source.Substring(
    $start[0].Extent.StartOffset, $stop[0].Extent.StartOffset - $start[0].Extent.StartOffset))
. (Join-Path $PSScriptRoot '../windows/app-build-plan.ps1')
$fixture = Join-Path ([IO.Path]::GetTempPath()) ('tilo-app-plan-' + [guid]::NewGuid().ToString('N'))
[void][IO.Directory]::CreateDirectory($fixture)
try {
    $binary = Join-Path $fixture 'fixture.exe'
    [IO.File]::WriteAllText($binary, 'test artifact; never executed')
    foreach ($selection in @(@('calculator', 'notes'), @('clocks', 'calculator', 'clocks'))) {
        $Apps = $selection
        $Profile = 'dev'
        $Jobs = 7
        $repository = $fixture
        $calls = [Collections.Generic.List[object]]::new()
        function cargo {
            $call = @($args)
            $calls.Add($call)
            Set-Variable -Name LASTEXITCODE -Value 0 -Scope 1
            $names = @()
            if ($call -contains '--example') { $names = @('windows-host') }
            else {
                for ($i = 0; $i -lt $call.Count; $i++) {
                    if ($call[$i] -eq '-p') { $names += $call[$i + 1] }
                }
            }
            foreach ($name in $names) {
                @{ reason = 'compiler-artifact'; target = @{ name = $name }; executable = $binary } | ConvertTo-Json -Compress
            }
        }
        & $productionBuild
        $planned = Get-TiloWindowsAppBuildPlan -Apps $Apps -Profile $Profile -Jobs $Jobs
        $expected = @($planned.applicationBuild)
        if ($planned.notificationBuild) { $expected += $planned.notificationBuild }
        if ($calls.Count -ne $expected.Count) { throw 'Production does not execute exactly the planned builds.' }
        for ($i = 0; $i -lt $calls.Count; $i++) {
            if (($calls[$i] -join ' ') -ne ($expected[$i].arguments -join ' ') -or
                ($calls[$i] -join ' ') -notmatch '--profile dev --jobs 7') {
                throw 'Production changed or ignored the planned Cargo arguments.'
            }
        }
    }
} finally {
    Remove-Item Function:cargo -ErrorAction SilentlyContinue
    if ([IO.Path]::GetDirectoryName([IO.Path]::GetFullPath($fixture)) -ne
        [IO.Path]::GetFullPath([IO.Path]::GetTempPath()).TrimEnd([IO.Path]::DirectorySeparatorChar)) {
        throw 'Refusing to remove a fixture outside the temporary directory.'
    }
    [IO.Directory]::Delete($fixture, $true)
}
Write-Output 'PASS: Windows app plans match production builds, runtime requirements and native selection without compiling or installing.'

# Pure build/runtime description shared by the read-only planner and installer.
# Callers validate Apps against the installer's supported application contract.
function Get-TiloWindowsAppBuildPlan {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)][ValidateNotNullOrEmpty()][string[]]$Apps,
        [ValidateSet('dev', 'release')][string]$Profile = 'release',
        [ValidateRange(1, 256)][int]$Jobs = 2
    )

    $applications = @($Apps | Select-Object -Unique)
    $common = @('build', '--locked', '--profile', $Profile, '--jobs', "$Jobs", '--message-format=json-render-diagnostics')
    $arguments = @($common)
    foreach ($application in $applications) { $arguments += @('-p', $application) }
    # Workspace builds have the patched wgpu-hal the DX12 shader cache needs.
    $arguments += @('--features', 'ui/dx12-shader-cache')
    $applicationBuild = [ordered]@{ executable = 'cargo'; arguments = $arguments }
    $notificationBuild = $null
    if ('clocks' -in $applications) {
        $notificationBuild = [ordered]@{
            executable = 'cargo'
            arguments = @($common) + @('-p', 'ui', '--example', 'windows-host', '--features', 'notifications')
        }
    }
    [pscustomobject]@{
        applications = $applications
        applicationBuild = $applicationBuild
        notificationBuild = $notificationBuild
        runtime = [ordered]@{
            fonts = 'crates/sdk/ui-core/assets/fonts'
            dxc = $true
            pdfium = ('pdf-viewer' -in $applications -or 'file-explorer' -in $applications)
            gstreamer = 'Resolve from built executables PE imports; no media imports means no bundle.'
            notifications = ('clocks' -in $applications)
        }
    }
}

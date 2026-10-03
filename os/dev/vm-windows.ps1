# Windows ARM64 transport for `tilo-vm scenario install`.
# The installer action driver lives in Rust; this only manages its isolated
# WHPX QEMU process. The source medium is never opened for writes.
param([Parameter(Position=0)][string]$Action, [Parameter(ValueFromRemainingArguments=$true)][string[]]$Args)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

function Get-ScenarioNetwork {
    # Optional recovery-login test endpoint; never expose the guest on the LAN.
    $port = $env:TILO_SCENARIO_SSH_PORT
    if (!$port) { return 'user,id=net0' }
    $number = 0
    if ($port -notmatch '^[0-9]{1,5}$' -or ![int]::TryParse($port, [ref]$number) -or $number -lt 1024 -or $number -gt 65535) {
        throw 'TILO_SCENARIO_SSH_PORT must be an unprivileged TCP port (1024-65535)'
    }
    return "user,id=net0,hostfwd=tcp:127.0.0.1:$number-:22"
}

$cycle = $env:TILO_VM_CYCLE
$storage = $env:TILO_INSTALLER_OUTPUT
$medium = $env:TILO_INSTALLER_DISK
$run = $env:TILO_RUN
if (!$cycle -or !$storage -or !$medium -or !$run -or !$env:TILO_CONSOLE_PORT -or !$env:TILO_QMP_PORT) {
    throw 'tilo-vm must supply cycle, storage, medium, runtime and TCP ports'
}
if ($cycle -notmatch '^scenario-install-[0-9TZ-]+-[0-9]+$') { throw 'invalid scenario cycle name' }
$cycleDir = Join-Path (Join-Path $storage 'vm-cycles') $cycle
$externalTarget = $env:TILO_INSTALL_TARGET_DISK
$target = if ($externalTarget) { [IO.Path]::GetFullPath($externalTarget) } else { Join-Path $cycleDir 'target.qcow2' }
$targetFormat = if ($externalTarget) { 'raw' } else { 'qcow2' }
$work = Join-Path $cycleDir 'installer-work.qcow2'
$vars = Join-Path $cycleDir 'vars.fd'
$pidFile = Join-Path $run 'qemu.pid'
$serial = Join-Path $run 'serial.log'
$uart = Join-Path $run 'uart.log'
$proof = Join-Path $run 'install-proof.log'

function Resolve-Tool([string]$name) {
    $override = switch ($name) {
        'qemu-img' { $env:TILO_QEMU_IMG }
        'qemu-system-aarch64' { $env:TILO_QEMU_SYSTEM }
        'magick' { $env:TILO_MAGICK }
        default { $null }
    }
    if ($override) { return (Resolve-Path -LiteralPath $override).Path }
    $cmd = Get-Command "$name.exe" -ErrorAction SilentlyContinue
    if ($cmd) { return $cmd.Source }
    return (Resolve-Path -LiteralPath (Join-Path $env:USERPROFILE "msys64/clangarm64/bin/$name.exe")).Path
}

function Resolve-Python {
    if ($env:TILO_PYTHON) { return (Resolve-Path -LiteralPath $env:TILO_PYTHON).Path }
    foreach ($name in @('python3.exe', 'python.exe')) {
        $candidate = Get-Command $name -CommandType Application -ErrorAction SilentlyContinue
        if ($candidate -and $candidate.Source -notlike '*\WindowsApps\*') { return $candidate.Source }
    }
    throw 'Installer code refresh requires Python 3 on PATH, or TILO_PYTHON pointing to its executable'
}

function Normal-ScenarioPath([string]$path) {
    $full = [IO.Path]::GetFullPath($path)
    if ($full.StartsWith('\\?\')) { return $full.Substring(4) }
    return $full
}

function Resolve-InstallerBootstrap([string]$img) {
    if (!$env:TILO_INSTALLER_BOOTSTRAP_DISK) { return $medium }
    if (!$env:TILO_INSTALLER_CODE_FROM -or $env:TILO_INSTALLER_CODE_PAUSE -ne '1') {
        throw 'Local bootstrap requires explicit CODE_FROM and CODE_PAUSE=1 development opt-ins'
    }
    $bootstrap = (Resolve-Path -LiteralPath $env:TILO_INSTALLER_BOOTSTRAP_DISK).Path
    $record = Get-Content -LiteralPath ($bootstrap + '.bootstrap.json') -Raw | ConvertFrom-Json
    $cached = $record.PSObject.Properties['cached_source']
    $budget = if ($cached) { 128MB } else { 64MB }
    if ($record.schema -ne 'tilo.installer-code-bootstrap.v1' -or $record.physical_admission -ne $false -or
        $record.overlay -ne [IO.Path]::GetFileName($bootstrap) -or $record.overlay_sha256 -notmatch '^[0-9a-f]{64}$' -or
        $record.changed_extent_bytes -gt $budget -or $record.changed_extent_bytes -le 0) {
        throw 'Invalid VM-only bootstrap provenance'
    }
    if ((Get-FileHash -LiteralPath $bootstrap -Algorithm SHA256).Hash.ToLowerInvariant() -ne $record.overlay_sha256) {
        throw 'Bootstrap disk differs from its host receipt'
    }
    $chain = (& $img info --output=json --backing-chain -f qcow2 $bootstrap | ConvertFrom-Json)
    $expectedCount = if ($cached) { $record.cached_source.chain.Count + 1 } else { 2 }
    if ($LASTEXITCODE -or $chain.Count -ne $expectedCount -or $expectedCount -lt 2 -or $expectedCount -gt 5 -or
        ![String]::Equals((Normal-ScenarioPath $chain[-1].filename), (Normal-ScenarioPath $medium), [StringComparison]::OrdinalIgnoreCase) -or
        @($chain | Where-Object { $_.format -ne 'qcow2' -or $_.'virtual-size' -gt $chain[0].'virtual-size' -or
            $chain[0].'virtual-size' - $_.'virtual-size' -gt 64MB }).Count) {
        throw 'Bootstrap chain does not end at the selected immutable CI medium'
    }
    if ($cached) {
        $cache = Get-Item -LiteralPath $chain[1].filename
        $recordedCache = Normal-ScenarioPath (Join-Path (Split-Path $bootstrap -Parent) $record.backing)
        $modified = ([DateTimeOffset]$cache.LastWriteTimeUtc).ToUnixTimeMilliseconds()
        if ($record.cached_source.kind -ne 'unverified-retained-scenario-work' -or $cache.Name -ne 'installer-work.qcow2' -or
            $cache.Length -ne $record.cached_source.bytes -or
            $modified -ne [long][Math]::Floor($record.cached_source.mtime_ns / 1000000) -or
            ![String]::Equals((Normal-ScenarioPath $cache.FullName), $recordedCache, [StringComparison]::OrdinalIgnoreCase)) {
            throw 'Explicit unverified scenario cache differs from its bootstrap snapshot'
        }
    }
    $ciReceipt = Get-Content -LiteralPath ($medium + '.verified.json') -Raw | ConvertFrom-Json
    if ($ciReceipt.sha256 -ne $record.base_sha256 -or $ciReceipt.bytes -ne $record.base_bytes) {
        throw 'Bootstrap base differs from the verified CI download receipt'
    }
    Copy-Item -LiteralPath ($bootstrap + '.bootstrap.json') -Destination (Join-Path $cycleDir 'installer-bootstrap.json')
    return $bootstrap
}

function Resolve-Firmware([string]$name, [string]$override) {
    if ($override) { return (Resolve-Path -LiteralPath $override).Path }
    $qemu = Resolve-Tool 'qemu-system-aarch64'
    $root = Split-Path (Split-Path $qemu -Parent) -Parent
    return (Resolve-Path -LiteralPath (Join-Path $root "share/qemu/$name")).Path
}

function Get-VmProcess {
    if (!(Test-Path -LiteralPath $pidFile)) { return $null }
    # A reused PID must never let status or stop act on another QEMU process.
    try {
        $identity = Get-Content -LiteralPath $pidFile -Raw | ConvertFrom-Json
        if ($identity.cycle -ne $cycle) { return $null }
        $proc = Get-Process -Id ([int]$identity.id) -ErrorAction SilentlyContinue
        if ($proc -and $proc.ProcessName -eq 'qemu-system-aarch64' -and
            $proc.StartTime.ToUniversalTime().Ticks -eq [long]$identity.started) { return $proc }
    } catch { return $null }
    return $null
}

function Invoke-Qmp([string]$name, [object]$arguments = $null) {
    $client = [Net.Sockets.TcpClient]::new()
    try {
        $client.Connect('127.0.0.1', [int]$env:TILO_QMP_PORT)
        $client.ReceiveTimeout = 3000
        $stream = $client.GetStream()
        $reader = [IO.StreamReader]::new($stream)
        $writer = [IO.StreamWriter]::new($stream)
        $writer.AutoFlush = $true
        $null = $reader.ReadLine() # greeting
        $writer.WriteLine('{"execute":"qmp_capabilities"}')
        do { $reply = $reader.ReadLine() | ConvertFrom-Json } while ($reply.PSObject.Properties['event'])
        if ($reply.PSObject.Properties['error']) { throw ($reply.error | ConvertTo-Json -Compress) }
        $command = @{ execute = $name }
        if ($null -ne $arguments) { $command.arguments = $arguments }
        $writer.WriteLine(($command | ConvertTo-Json -Compress -Depth 8))
        do { $reply = $reader.ReadLine() | ConvertFrom-Json } while ($reply.PSObject.Properties['event'])
        if ($reply.PSObject.Properties['error']) { throw ($reply.error | ConvertTo-Json -Compress) }
        return $reply
    } finally { $client.Dispose() }
}

function Send-Key([string]$key) {
    $null = Invoke-Qmp 'human-monitor-command' @{ 'command-line' = "sendkey $key 20" }
    Start-Sleep -Milliseconds 80
}

function Boot([bool]$installer, [string[]]$flags) {
    if (Get-VmProcess) { throw 'scenario VM is already running' }
    if (!(Test-Path -LiteralPath $medium)) { throw "installer medium missing: $medium" }
    $free = [IO.DriveInfo]::new([IO.Path]::GetPathRoot($cycleDir)).AvailableFreeSpace
    if ($free -lt 15GB) { throw 'WHPX scenario requires at least 15 GiB free on its disk volume' }
    if ($flags -contains '--tpm') { throw 'WHPX scenario has no persistent software TPM' }
    if ($flags -contains '--gl') { throw 'WHPX scenario has no configured virgl headless backend' }
    if ($externalTarget) {
        $file = Get-Item -LiteralPath $target -ErrorAction Stop
        if ($file.PSIsContainer -or ($file.Attributes -band [IO.FileAttributes]::ReparsePoint) -or
            $file.Extension -ne '.raw' -or $file.Length -lt 40GB) {
            throw 'external target must be a regular sparse raw file of at least 40 GiB'
        }
    }
    $qemu = Resolve-Tool 'qemu-system-aarch64'
    $img = Resolve-Tool 'qemu-img'
    $bootMedium = if ($installer) { Resolve-InstallerBootstrap $img } else { $medium }
    if ($installer) {
        if ((Test-Path -LiteralPath $proof) -and (Get-Item -LiteralPath $proof).Length -ne 0) {
            throw "stale installer proof file already contains evidence: $proof"
        }
        if (!(Test-Path -LiteralPath $work)) { & $img create -f qcow2 -F qcow2 -b $bootMedium $work | Out-Null; if ($LASTEXITCODE) { throw 'creating installer overlay failed' } }
        if (!(Test-Path -LiteralPath $target)) { & $img create -f qcow2 $target 40G | Out-Null; if ($LASTEXITCODE) { throw 'creating sparse target failed' } }
        $disks = @('-drive', "if=none,id=medium,format=qcow2,file=$work,cache=writeback,aio=threads", '-device', 'virtio-blk-pci,drive=medium,serial=tilo-installer-medium,bootindex=1', '-drive', "if=none,id=target,format=$targetFormat,file=$target,cache=writeback,aio=threads", '-device', 'virtio-blk-pci,drive=target,serial=tilo-install-target')
    } else {
        if (!(Test-Path -LiteralPath $target)) { throw 'installed target is missing' }
        $disks = @('-drive', "if=none,id=target,format=$targetFormat,file=$target,cache=writeback,aio=threads", '-device', 'virtio-blk-pci,drive=target,serial=tilo-install-target,bootindex=1')
    }
    if ($env:TILO_INSTALLER_PROBE_UKI) {
        if (!$installer -or !$env:TILO_INSTALLER_BOOTSTRAP_DISK -or !$env:TILO_INSTALLER_CODE_FROM -or $env:TILO_INSTALLER_CODE_PAUSE -ne '1') {
            throw 'Read-only UKI probe fixture requires an explicitly paused local bootstrap scenario'
        }
        $fixture = Get-Item -LiteralPath $env:TILO_INSTALLER_PROBE_UKI
        if ($fixture.PSIsContainer -or ($fixture.Attributes -band [IO.FileAttributes]::ReparsePoint) -or $fixture.Extension -ne '.tar' -or $fixture.Length -lt 1MB -or $fixture.Length -gt 512MB -or $fixture.Length % 512) {
            throw 'UKI probe fixture must be a regular 1-512 MiB sector-aligned boot-input tar file'
        }
        $fixtureRecord = @{ schema = 'tilo.installer-code-uki-fixture.v1'; physical_admission = $false;
            source_kind = 'operator-supplied-original-ci-input'; path = $fixture.FullName; bytes = $fixture.Length;
            sha256 = (Get-FileHash -LiteralPath $fixture.FullName -Algorithm SHA256).Hash.ToLowerInvariant(); readonly = $true }
        $fixtureRecord | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $cycleDir 'installer-code-uki-fixture.json')
        $disks += @('-drive', "if=none,id=codefixture,format=raw,readonly=on,file=$($fixture.FullName)", '-device', 'virtio-blk-pci,drive=codefixture,serial=tilo-code-fixture')
    }
    if (!(Test-Path -LiteralPath $vars)) {
        $template = Resolve-Firmware 'edk2-arm-vars.fd' $env:TILO_EFI_VARS
        Copy-Item -LiteralPath $template -Destination $vars
    }
    $code = Resolve-Firmware 'edk2-aarch64-code.fd' $env:TILO_EFI_CODE
    # ProcessStartInfo.ArgumentList keeps paths and comma-bearing QEMU options intact.
    @{ schema = 'tilo.installer-code-runtime.v1'; enabled = [bool]($installer -and $env:TILO_INSTALLER_CODE_FROM); port = $null; source = $null } |
        ConvertTo-Json | Set-Content -LiteralPath (Join-Path $cycleDir 'installer-code-channel.json')
    $codePort = @()
    $codeNumber = 0
    if ($installer -and $env:TILO_INSTALLER_CODE_FROM) {
        @{ phase = 'startup' } | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $cycleDir 'installer-code-phase.json')
        $listener = [Net.Sockets.TcpListener]::new([Net.IPAddress]::Loopback, 0)
        $listener.Start()
        $codeNumber = ([Net.IPEndPoint]$listener.LocalEndpoint).Port
        $listener.Stop()
        $codePort = @('-chardev', "socket,id=tilocode,host=127.0.0.1,port=$codeNumber,server=on,wait=off", '-device', 'virtserialport,chardev=tilocode,name=org.tilo.installer-code')
        @{ schema = 'tilo.installer-code-runtime.v1'; enabled = $true; port = $codeNumber; source = [IO.Path]::GetFullPath($env:TILO_INSTALLER_CODE_FROM) } |
            ConvertTo-Json | Set-Content -LiteralPath (Join-Path $cycleDir 'installer-code-channel.json')
    }
    $proofPort = if ($installer) { @('-chardev', "file,id=tiloproof,path=$proof", '-device', 'virtserialport,chardev=tiloproof,name=org.tilo.vm-install-proof') } else { @() }
    $argv = @('-name', $cycle, '-machine', 'virt,gic-version=3,highmem=on', '-accel', 'whpx', '-cpu', 'host', '-smp', 'cpus=4', '-m', '3072', '-no-reboot', '-drive', "if=pflash,format=raw,readonly=on,file=$code", '-drive', "if=pflash,format=raw,file=$vars") + $disks + @('-device', 'virtio-gpu-pci,edid=on,xres=1280,yres=1024', '-device', 'qemu-xhci', '-device', 'usb-kbd', '-device', 'usb-tablet', '-netdev', (Get-ScenarioNetwork), '-device', 'virtio-net-pci,netdev=net0', '-display', 'none', '-serial', "file:$uart", '-chardev', "file,id=tilo-ttyS0,path=$serial", '-device', 'pci-serial,chardev=tilo-ttyS0', '-device', 'virtio-serial-pci', '-chardev', "socket,id=tiloconsole,host=127.0.0.1,port=$env:TILO_CONSOLE_PORT,server=on,wait=off", '-device', 'virtserialport,chardev=tiloconsole,name=org.tilo.console.0') + $proofPort + $codePort + @('-qmp', "tcp:127.0.0.1:$env:TILO_QMP_PORT,server=on,wait=off", '-monitor', 'none')
    $start = [Diagnostics.ProcessStartInfo]::new($qemu)
    foreach ($arg in $argv) { $start.ArgumentList.Add([string]$arg) }
    # Command::output in tilo-vm captures this PowerShell process's stdout and
    # stderr. Detach QEMU from those pipes so boot-installer returns while the
    # VM is running, rather than waiting for QEMU to exit before driving it.
    $start.UseShellExecute = $true
    $start.WindowStyle = [Diagnostics.ProcessWindowStyle]::Hidden
    $proc = [Diagnostics.Process]::Start($start)
    @{ id = $proc.Id; started = $proc.StartTime.ToUniversalTime().Ticks.ToString(); cycle = $cycle } |
        ConvertTo-Json -Compress | Set-Content -LiteralPath $pidFile
    Start-Sleep -Seconds 1
    if ($proc.HasExited) { throw "QEMU exited immediately with code $($proc.ExitCode)" }
    if ($codeNumber) {
        $helper = Join-Path $PSScriptRoot 'refresh-installer-code.py'
        $python = Resolve-Python
        & $python -X utf8 $helper --source $env:TILO_INSTALLER_CODE_FROM --port $codeNumber --receipt (Join-Path $cycleDir 'installer-code-refresh.json')
        if ($LASTEXITCODE) { $owned = Get-VmProcess; if ($owned) { Stop-Process -Id $owned.Id }; throw 'initial installer code refresh failed' }
    }
    "running pid=$($proc.Id) target=$target"
}

switch ($Action) {
    'init-cycle' {
        if (Get-VmProcess) { throw 'VM is still running' }
        if (Test-Path -LiteralPath $cycleDir) { throw "cycle already exists: $cycleDir" }
        New-Item -ItemType Directory -Path $cycleDir, $run -Force | Out-Null
        "fresh VM cycle: $cycle"
    }
    'boot-installer' { Boot $true $Args }
    'boot-target' { Boot $false $Args }
    'discard-installer' {
        if (Get-VmProcess) { throw 'Installer VM is still running; refusing to discard its overlay' }
        if (!$externalTarget) { throw 'Discarding the installer requires an independent external raw target' }
        $resolvedWork = [IO.Path]::GetFullPath($work)
        $resolvedCycle = [IO.Path]::GetFullPath($cycleDir)
        if (!$resolvedWork.StartsWith($resolvedCycle + '\', [StringComparison]::OrdinalIgnoreCase)) {
            throw 'Installer overlay path is outside its owned cycle'
        }
        if (Test-Path -LiteralPath $resolvedWork) {
            $item = Get-Item -LiteralPath $resolvedWork
            if ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) { throw 'Refusing installer overlay reparse point' }
            Remove-Item -LiteralPath $resolvedWork
        }
        'finished installer overlay discarded; installed target retained'
    }
    'status' { if (Get-VmProcess) { 'running' } else { 'stopped' } }
    'stop-cache' {
        # Capture reusable cache only after a guest-controlled poweroff. QMP
        # quit/host termination cannot flush the guest filesystem journal.
        $proc = Get-VmProcess
        if (!$proc) { throw 'Cache capture requires the owned running scenario VM' }
        $identity = Invoke-Qmp 'query-name'
        if ($identity.return.name -ne $cycle) { throw 'QMP endpoint belongs to another VM' }
        $response = Invoke-Qmp 'system_powerdown'
        $deadline = [DateTime]::UtcNow.AddSeconds(45)
        while ((Get-VmProcess) -and [DateTime]::UtcNow -lt $deadline) { Start-Sleep -Milliseconds 100 }
        if (Get-VmProcess) { throw 'Guest did not power off cleanly; cache capture refused, VM left running' }
        @{ schema='tilo.installer-cache-shutdown.v1'; cycle=$cycle; process=$proc.Id;
           method='guest-system-powerdown'; physical_admission=$false; qmp=$response } |
            ConvertTo-Json -Depth 6 | Set-Content -LiteralPath (Join-Path $cycleDir 'installer-cache-shutdown.json')
    }
    'stop' {
        $proc = Get-VmProcess
        if ($proc) {
            try {
                $identity = Invoke-Qmp 'query-name'
                if ($identity.return.name -ne $cycle) { throw 'QMP endpoint belongs to another VM' }
                $null = Invoke-Qmp 'quit'
            } catch { Write-Warning "QMP quit did not acknowledge: $_" }
            # QMP quit can finish before Wait-Process attaches, which turns a
            # successful stop into ProcessAlreadyTerminated under Stop policy.
            try { $null = $proc.WaitForExit(5000) }
            catch [System.InvalidOperationException] { }
            $remaining = Get-VmProcess
            if ($remaining -and $remaining.Id -eq $proc.Id) {
                Stop-Process -Id $proc.Id -Force
            }
        }
    }
    'keys' { foreach ($key in $Args) { Send-Key $key } }
    'shot' {
        if ($Args.Count -ne 1 -or $Args[0] -notmatch '^[a-z0-9-]+$') { throw 'shot requires one safe name' }
        if (!(Get-VmProcess)) { throw 'scenario VM is not running' }
        $ppm = Join-Path $run "$($Args[0]).ppm"
        $png = Join-Path $run "$($Args[0]).png"
        $null = Invoke-Qmp 'screendump' @{ filename = $ppm }
        if (!(Test-Path -LiteralPath $ppm)) { throw 'QEMU screendump did not write a frame' }
        & (Resolve-Tool 'magick') $ppm $png
        if ($LASTEXITCODE -ne 0 -or !(Test-Path -LiteralPath $png)) { throw 'screendump conversion failed' }
        $png
    }
    'type' {
        if ($Args.Count -ne 1 -or $Args[0] -ne '-') { throw 'type accepts stdin only' }
        $value = [Console]::In.ReadToEnd().TrimEnd("`r", "`n")
        foreach ($char in $value.ToCharArray()) {
            $key = switch -CaseSensitive ([string]$char) {
                '-' { 'minus' }; '_' { 'shift-minus' }; '.' { 'dot' }; '/' { 'slash' }
                default { [string]$char }
            }
            Send-Key $key
        }
    }
    'tool' {
        if ($Args.Count -lt 1 -or $Args[0] -ne 'qemu-img') { throw 'Windows scenario tool supports qemu-img only' }
        $img = Resolve-Tool 'qemu-img'
        & $img @($Args | Select-Object -Skip 1)
        if ($LASTEXITCODE) { throw "qemu-img failed: $LASTEXITCODE" }
    }
    default { throw "unknown Windows scenario action: $Action" }
}

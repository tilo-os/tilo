# Windows ARM64 transport for `tilo-vm scenario install`.
# The installer action driver lives in Rust; this only manages its isolated
# WHPX QEMU process. The source medium is never opened for writes.
param([Parameter(Position=0)][string]$Action, [Parameter(ValueFromRemainingArguments=$true)][string[]]$Args)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

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
    $override = if ($name -eq 'qemu-img') { $env:TILO_QEMU_IMG } else { $env:TILO_QEMU_SYSTEM }
    if ($override) { return (Resolve-Path -LiteralPath $override).Path }
    $cmd = Get-Command "$name.exe" -ErrorAction SilentlyContinue
    if ($cmd) { return $cmd.Source }
    return (Resolve-Path -LiteralPath (Join-Path $env:USERPROFILE "msys64/clangarm64/bin/$name.exe")).Path
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
    if ($installer) {
        if (Test-Path -LiteralPath $proof) { throw "stale installer proof file already exists: $proof" }
        if (!(Test-Path -LiteralPath $work)) { & $img create -f qcow2 -F qcow2 -b $medium $work | Out-Null; if ($LASTEXITCODE) { throw 'creating installer overlay failed' } }
        if (!(Test-Path -LiteralPath $target)) { & $img create -f qcow2 $target 40G | Out-Null; if ($LASTEXITCODE) { throw 'creating sparse target failed' } }
        $disks = @('-drive', "if=none,id=medium,format=qcow2,file=$work,cache=writeback,aio=threads", '-device', 'virtio-blk-pci,drive=medium,serial=tilo-installer-medium,bootindex=1', '-drive', "if=none,id=target,format=$targetFormat,file=$target,cache=writeback,aio=threads", '-device', 'virtio-blk-pci,drive=target,serial=tilo-install-target')
    } else {
        if (!(Test-Path -LiteralPath $target)) { throw 'installed target is missing' }
        $disks = @('-drive', "if=none,id=target,format=$targetFormat,file=$target,cache=writeback,aio=threads", '-device', 'virtio-blk-pci,drive=target,serial=tilo-install-target,bootindex=1')
    }
    if (!(Test-Path -LiteralPath $vars)) {
        $template = Resolve-Firmware 'edk2-arm-vars.fd' $env:TILO_EFI_VARS
        Copy-Item -LiteralPath $template -Destination $vars
    }
    $code = Resolve-Firmware 'edk2-aarch64-code.fd' $env:TILO_EFI_CODE
    # ProcessStartInfo.ArgumentList keeps paths and comma-bearing QEMU options intact.
    $proofPort = if ($installer) { @('-chardev', "file,id=tiloproof,path=$proof", '-device', 'virtserialport,chardev=tiloproof,name=org.tilo.vm-install-proof') } else { @() }
    $argv = @('-name', $cycle, '-machine', 'virt,gic-version=3,highmem=on', '-accel', 'whpx', '-cpu', 'host', '-smp', 'cpus=4', '-m', '8192', '-no-reboot', '-drive', "if=pflash,format=raw,readonly=on,file=$code", '-drive', "if=pflash,format=raw,file=$vars") + $disks + @('-device', 'virtio-gpu-pci,edid=on,xres=1280,yres=1024', '-device', 'qemu-xhci', '-device', 'usb-kbd', '-device', 'usb-tablet', '-netdev', 'user,id=net0', '-device', 'virtio-net-pci,netdev=net0', '-display', 'none', '-serial', "file:$uart", '-chardev', "file,id=tilo-ttyS0,path=$serial", '-device', 'pci-serial,chardev=tilo-ttyS0', '-device', 'virtio-serial-pci', '-chardev', "socket,id=tiloconsole,host=127.0.0.1,port=$env:TILO_CONSOLE_PORT,server=on,wait=off", '-device', 'virtserialport,chardev=tiloconsole,name=org.tilo.console.0') + $proofPort + @('-qmp', "tcp:127.0.0.1:$env:TILO_QMP_PORT,server=on,wait=off", '-monitor', 'none')
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
    'status' { if (Get-VmProcess) { 'running' } else { 'stopped' } }
    'stop' {
        $proc = Get-VmProcess
        if ($proc) {
            try {
                $identity = Invoke-Qmp 'query-name'
                if ($identity.return.name -ne $cycle) { throw 'QMP endpoint belongs to another VM' }
                $null = Invoke-Qmp 'quit'
            } catch { Write-Warning "QMP quit did not acknowledge: $_" }
            Wait-Process -Id $proc.Id -Timeout 5 -ErrorAction SilentlyContinue
            $remaining = Get-VmProcess
            if ($remaining -and $remaining.Id -eq $proc.Id) {
                Stop-Process -Id $proc.Id -Force
            }
        }
    }
    'keys' { foreach ($key in $Args) { Send-Key $key } }
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

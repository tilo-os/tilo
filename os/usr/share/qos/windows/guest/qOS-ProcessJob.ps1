<#
Creates a qOS-owned Windows Job Object, starts one exact executable suspended,
assigns it before its first instruction can run, and waits until the whole job
has no active processes.

This source is installed only from the read-only qOS guest-tools ISO into an
administrator-owned directory.  The launch methods deliberately expose the
few typed contracts used by the focused app, installer, and uninstaller
scripts; none invokes cmd.exe, PowerShell, ShellExecute, or a registry command.

The job contains normal CreateProcess descendants and deliberately denies
breakaway. Windows cannot attach work that an application asks an already
running service, WMI provider, scheduled task, or other out-of-process broker
to create; software that requires such delegation or explicit breakaway may
outlive the visible tree or fail. Windows 8/Server 2012 or newer is also needed
when Task Scheduler has already put the helper in a parent job.
#>
$ErrorActionPreference = 'Stop'

if ($null -eq ('qOS.WindowsGuest.ProcessJob' -as [type])) {
    Add-Type -TypeDefinition @'
using System;
using System.ComponentModel;
using System.IO;
using System.Runtime.InteropServices;
using System.Text;
using System.Threading;

namespace qOS.WindowsGuest
{
    public sealed class ProcessElevationRequiredException : InvalidOperationException
    {
        internal ProcessElevationRequiredException(string message) : base(message) { }
    }

    public sealed class ProcessJobResult
    {
        public int ProcessId { get; private set; }
        public int ExitCode { get; private set; }

        internal ProcessJobResult(uint processId, uint exitCode)
        {
            ProcessId = unchecked((int)processId);
            ExitCode = unchecked((int)exitCode);
        }
    }

    public static class ProcessJob
    {
        private const uint CreateSuspended = 0x00000004;
        private const uint JobObjectLimitKillOnJobClose = 0x00002000;
        private const int JobObjectBasicAccountingInformationClass = 1;
        private const int JobObjectExtendedLimitInformationClass = 9;
        private const uint ResumeFailed = 0xffffffff;
        private const int ErrorAccessDenied = 5;
        private const int ErrorElevationRequired = 740;
        private const int MaxUninstallArguments = 32;
        private const int MaxUninstallArgumentChars = 4096;
        private const int MaxUninstallCommandChars = 16384;

        [StructLayout(LayoutKind.Sequential)]
        private struct StartupInfo
        {
            public uint cb;
            public IntPtr lpReserved;
            public IntPtr lpDesktop;
            public IntPtr lpTitle;
            public uint dwX;
            public uint dwY;
            public uint dwXSize;
            public uint dwYSize;
            public uint dwXCountChars;
            public uint dwYCountChars;
            public uint dwFillAttribute;
            public uint dwFlags;
            public ushort wShowWindow;
            public ushort cbReserved2;
            public IntPtr lpReserved2;
            public IntPtr hStdInput;
            public IntPtr hStdOutput;
            public IntPtr hStdError;
        }

        [StructLayout(LayoutKind.Sequential)]
        private struct ProcessInformation
        {
            public IntPtr hProcess;
            public IntPtr hThread;
            public uint dwProcessId;
            public uint dwThreadId;
        }

        [StructLayout(LayoutKind.Sequential)]
        private struct JobObjectBasicLimitInformation
        {
            public long PerProcessUserTimeLimit;
            public long PerJobUserTimeLimit;
            public uint LimitFlags;
            public UIntPtr MinimumWorkingSetSize;
            public UIntPtr MaximumWorkingSetSize;
            public uint ActiveProcessLimit;
            public UIntPtr Affinity;
            public uint PriorityClass;
            public uint SchedulingClass;
        }

        [StructLayout(LayoutKind.Sequential)]
        private struct IoCounters
        {
            public ulong ReadOperationCount;
            public ulong WriteOperationCount;
            public ulong OtherOperationCount;
            public ulong ReadTransferCount;
            public ulong WriteTransferCount;
            public ulong OtherTransferCount;
        }

        [StructLayout(LayoutKind.Sequential)]
        private struct JobObjectExtendedLimitInformation
        {
            public JobObjectBasicLimitInformation BasicLimitInformation;
            public IoCounters IoInfo;
            public UIntPtr ProcessMemoryLimit;
            public UIntPtr JobMemoryLimit;
            public UIntPtr PeakProcessMemoryUsed;
            public UIntPtr PeakJobMemoryUsed;
        }

        [StructLayout(LayoutKind.Sequential)]
        private struct JobObjectBasicAccountingInformation
        {
            public long TotalUserTime;
            public long TotalKernelTime;
            public long ThisPeriodTotalUserTime;
            public long ThisPeriodTotalKernelTime;
            public uint TotalPageFaultCount;
            public uint TotalProcesses;
            public uint ActiveProcesses;
            public uint TotalTerminatedProcesses;
        }

        [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
        private static extern IntPtr CreateJobObject(IntPtr jobAttributes, string name);

        [DllImport("kernel32.dll", SetLastError = true)]
        private static extern bool SetInformationJobObject(
            IntPtr job,
            int informationClass,
            ref JobObjectExtendedLimitInformation information,
            uint informationLength);

        [DllImport("kernel32.dll", SetLastError = true)]
        private static extern bool QueryInformationJobObject(
            IntPtr job,
            int informationClass,
            ref JobObjectBasicAccountingInformation information,
            uint informationLength,
            IntPtr returnLength);

        [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
        private static extern bool CreateProcess(
            string applicationName,
            StringBuilder commandLine,
            IntPtr processAttributes,
            IntPtr threadAttributes,
            bool inheritHandles,
            uint creationFlags,
            IntPtr environment,
            string currentDirectory,
            ref StartupInfo startupInfo,
            out ProcessInformation processInformation);

        [DllImport("kernel32.dll", SetLastError = true)]
        private static extern bool AssignProcessToJobObject(IntPtr job, IntPtr process);

        [DllImport("kernel32.dll", SetLastError = true)]
        private static extern uint ResumeThread(IntPtr thread);

        [DllImport("kernel32.dll", SetLastError = true)]
        private static extern bool GetExitCodeProcess(IntPtr process, out uint exitCode);

        [DllImport("kernel32.dll", SetLastError = true)]
        private static extern bool TerminateProcess(IntPtr process, uint exitCode);

        [DllImport("kernel32.dll", SetLastError = true)]
        private static extern bool CloseHandle(IntPtr handle);

        [DllImport("shell32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
        private static extern IntPtr CommandLineToArgvW(string commandLine, out int argumentCount);

        [DllImport("kernel32.dll", SetLastError = true)]
        private static extern IntPtr LocalFree(IntPtr memory);

        // The manifest's argument tail is passed directly to the selected
        // executable, never parsed by a shell. lpApplicationName remains the
        // independently validated full path, so arguments cannot select a
        // different program even when the tail begins with a quoted token.
        public static ProcessJobResult RunFocusedApplication(string executable, string argumentTail)
        {
            return Run(executable, argumentTail);
        }

        public static ProcessJobResult RunExecutableInstaller(string packagePath)
        {
            return Run(packagePath, null);
        }

        public static ProcessJobResult RunMsiInstaller(string msiexecPath, string packagePath)
        {
            return Run(msiexecPath, "/i " + QuoteArgument(FullPath(packagePath)));
        }

        public static ProcessJobResult RunMsiUninstaller(string msiexecPath, string productCode)
        {
            if (String.IsNullOrWhiteSpace(productCode))
            {
                throw new ArgumentException("A Windows Installer product code is required.", "productCode");
            }
            return Run(msiexecPath, "/x " + QuoteArgument(productCode));
        }

        public static ProcessJobResult RunExecutableUninstaller(string executablePath, string[] arguments)
        {
            return Run(executablePath, BuildArgumentTail(arguments));
        }

        // Registry uninstall strings are parsed once with Windows' native
        // argv rules during discovery. The raw string is never persisted or
        // executed and CommandLineToArgvW is not a shell or expansion engine.
        public static string[] ParseUninstallCommandLine(string commandLine)
        {
            if (String.IsNullOrWhiteSpace(commandLine) || commandLine.Length > MaxUninstallCommandChars)
            {
                throw new ArgumentException("The uninstall command line is empty or exceeds its bound.", "commandLine");
            }

            int argumentCount;
            IntPtr nativeArguments = CommandLineToArgvW(commandLine, out argumentCount);
            if (nativeArguments == IntPtr.Zero)
            {
                ThrowWin32("Could not parse the Windows uninstall command line.");
            }
            try
            {
                if (argumentCount < 1 || argumentCount > MaxUninstallArguments + 1)
                {
                    throw new ArgumentException("The uninstall command line has too many arguments.", "commandLine");
                }
                string[] arguments = new string[argumentCount];
                int totalChars = 0;
                for (int index = 0; index < argumentCount; index++)
                {
                    IntPtr argumentPointer = Marshal.ReadIntPtr(nativeArguments, index * IntPtr.Size);
                    string argument = Marshal.PtrToStringUni(argumentPointer);
                    if (argument == null || argument.Length > MaxUninstallArgumentChars || ContainsControlCharacter(argument))
                    {
                        throw new ArgumentException("An uninstall argument is invalid or exceeds its bound.", "commandLine");
                    }
                    totalChars += argument.Length;
                    if (totalChars > MaxUninstallCommandChars)
                    {
                        throw new ArgumentException("The parsed uninstall arguments exceed their aggregate bound.", "commandLine");
                    }
                    arguments[index] = argument;
                }
                return arguments;
            }
            finally
            {
                LocalFree(nativeArguments);
            }
        }

        private static ProcessJobResult Run(string executable, string argumentTail)
        {
            string applicationPath = FullPath(executable);
            if (!File.Exists(applicationPath))
            {
                throw new FileNotFoundException("The managed Windows executable does not exist.", applicationPath);
            }

            IntPtr job = IntPtr.Zero;
            ProcessInformation process = new ProcessInformation();
            bool processCreated = false;
            bool assigned = false;
            try
            {
                job = CreateJobObject(IntPtr.Zero, null);
                if (job == IntPtr.Zero)
                {
                    ThrowWin32("Could not create the qOS process job.");
                }

                // Closing this handle unexpectedly (for example when Task
                // Scheduler terminates the helper) tears down the contained
                // process tree instead of leaving invisible guest programs.
                // Deliberately do not enable BREAKAWAY_OK or
                // SILENT_BREAKAWAY_OK: normal CreateProcess descendants must
                // stay in this job.
                JobObjectExtendedLimitInformation limits = new JobObjectExtendedLimitInformation();
                limits.BasicLimitInformation.LimitFlags = JobObjectLimitKillOnJobClose;
                if (!SetInformationJobObject(
                    job,
                    JobObjectExtendedLimitInformationClass,
                    ref limits,
                    (uint)Marshal.SizeOf(typeof(JobObjectExtendedLimitInformation))))
                {
                    ThrowWin32("Could not configure the qOS process job.");
                }

                StartupInfo startup = new StartupInfo();
                startup.cb = (uint)Marshal.SizeOf(typeof(StartupInfo));
                StringBuilder commandLine = new StringBuilder(QuoteArgument(applicationPath));
                if (!String.IsNullOrEmpty(argumentTail))
                {
                    commandLine.Append(' ');
                    commandLine.Append(argumentTail);
                }

                if (!CreateProcess(
                    applicationPath,
                    commandLine,
                    IntPtr.Zero,
                    IntPtr.Zero,
                    false,
                    CreateSuspended,
                    IntPtr.Zero,
                    null,
                    ref startup,
                    out process))
                {
                    int error = Marshal.GetLastWin32Error();
                    if (error == ErrorElevationRequired)
                    {
                        throw new ProcessElevationRequiredException(
                            "The selected executable requires UAC elevation. qOS refuses to launch it outside process-tree containment.");
                    }
                    throw new Win32Exception(error, "Could not create the managed Windows process.");
                }
                processCreated = true;

                // The primary thread has never executed here, so neither the
                // bootstrap process nor a fast detached GUI child can outrun
                // assignment to the non-breakaway job.
                if (!AssignProcessToJobObject(job, process.hProcess))
                {
                    int error = Marshal.GetLastWin32Error();
                    TerminateProcess(process.hProcess, 1);
                    if (error == ErrorAccessDenied)
                    {
                        throw new InvalidOperationException(
                            "The process is already in an incompatible parent job. qOS refuses to resume it without descendant containment.");
                    }
                    throw new Win32Exception(error, "Could not contain the managed Windows process before launch.");
                }
                assigned = true;

                if (ResumeThread(process.hThread) == ResumeFailed)
                {
                    ThrowWin32("Could not resume the contained Windows process.");
                }

                WaitUntilEmpty(job);
                uint exitCode;
                if (!GetExitCodeProcess(process.hProcess, out exitCode))
                {
                    ThrowWin32("Could not read the managed Windows process exit code.");
                }
                return new ProcessJobResult(process.dwProcessId, exitCode);
            }
            finally
            {
                if (process.hThread != IntPtr.Zero)
                {
                    CloseHandle(process.hThread);
                }
                if (process.hProcess != IntPtr.Zero)
                {
                    // A failed assignment leaves a suspended process outside
                    // the job; terminate it explicitly before dropping its
                    // last local handle. Assigned trees are killed by closing
                    // the configured job on every exceptional path.
                    if (processCreated && !assigned)
                    {
                        TerminateProcess(process.hProcess, 1);
                    }
                    CloseHandle(process.hProcess);
                }
                if (job != IntPtr.Zero)
                {
                    CloseHandle(job);
                }
            }
        }

        private static void WaitUntilEmpty(IntPtr job)
        {
            // A job handle is not a general "empty" wait handle. Query the
            // kernel-maintained active count; zero is stable because no member
            // remains that could create another in-job descendant.
            while (true)
            {
                JobObjectBasicAccountingInformation accounting = new JobObjectBasicAccountingInformation();
                if (!QueryInformationJobObject(
                    job,
                    JobObjectBasicAccountingInformationClass,
                    ref accounting,
                    (uint)Marshal.SizeOf(typeof(JobObjectBasicAccountingInformation)),
                    IntPtr.Zero))
                {
                    ThrowWin32("Could not query the qOS process job.");
                }
                if (accounting.ActiveProcesses == 0)
                {
                    return;
                }
                Thread.Sleep(100);
            }
        }

        private static string FullPath(string path)
        {
            if (String.IsNullOrWhiteSpace(path))
            {
                throw new ArgumentException("A managed executable path is required.", "path");
            }
            if (!Path.IsPathRooted(path))
            {
                throw new ArgumentException("A managed executable path must be absolute.", "path");
            }
            return Path.GetFullPath(path);
        }

        // Quote one Windows argv token using the CommandLineToArgvW escaping
        // convention. Fixed MSI/package fields therefore remain one argument.
        private static string QuoteArgument(string argument)
        {
            if (argument == null)
            {
                return "\"\"";
            }
            StringBuilder quoted = new StringBuilder();
            quoted.Append('"');
            int slashes = 0;
            foreach (char character in argument)
            {
                if (character == '\\')
                {
                    slashes++;
                    continue;
                }
                if (character == '"')
                {
                    quoted.Append('\\', (slashes * 2) + 1);
                    quoted.Append('"');
                    slashes = 0;
                    continue;
                }
                quoted.Append('\\', slashes);
                slashes = 0;
                quoted.Append(character);
            }
            quoted.Append('\\', slashes * 2);
            quoted.Append('"');
            return quoted.ToString();
        }

        private static string BuildArgumentTail(string[] arguments)
        {
            if (arguments == null || arguments.Length == 0)
            {
                return null;
            }
            if (arguments.Length > MaxUninstallArguments)
            {
                throw new ArgumentException("The executable uninstaller has too many arguments.", "arguments");
            }
            StringBuilder tail = new StringBuilder();
            int totalChars = 0;
            for (int index = 0; index < arguments.Length; index++)
            {
                string argument = arguments[index];
                if (argument == null || argument.Length > MaxUninstallArgumentChars || ContainsControlCharacter(argument))
                {
                    throw new ArgumentException("An executable uninstaller argument is invalid or exceeds its bound.", "arguments");
                }
                totalChars += argument.Length;
                if (totalChars > MaxUninstallCommandChars)
                {
                    throw new ArgumentException("The executable uninstaller arguments exceed their aggregate bound.", "arguments");
                }
                if (index != 0)
                {
                    tail.Append(' ');
                }
                tail.Append(QuoteArgument(argument));
            }
            return tail.ToString();
        }

        private static bool ContainsControlCharacter(string value)
        {
            foreach (char character in value)
            {
                if (character < 0x20 || character == 0x7f)
                {
                    return true;
                }
            }
            return false;
        }

        private static void ThrowWin32(string message)
        {
            throw new Win32Exception(Marshal.GetLastWin32Error(), message);
        }
    }
}
'@ -Language CSharp
}

function Test-qOSPathInsideRoot {
    param(
        [Parameter(Mandatory = $true)][string]$Path,
        [Parameter(Mandatory = $true)][string]$Root
    )
    return $Path.StartsWith(($Root.TrimEnd('\') + '\'), [StringComparison]::OrdinalIgnoreCase)
}

function Assert-qOSTrustedPathAcl {
    param([Parameter(Mandatory = $true)][string]$Path)

    $item = Get-Item -LiteralPath $Path -Force -ErrorAction Stop
    if (($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
        throw "qOS executable uninstall binding crosses a reparse point: $Path"
    }

    $acl = Get-Acl -LiteralPath $Path -ErrorAction Stop
    $ownerSid = $acl.GetOwner([Security.Principal.SecurityIdentifier]).Value
    $trustedOwners = @(
        'S-1-5-18',       # Local System
        'S-1-5-32-544',   # BUILTIN\Administrators
        'S-1-5-80-956008885-3418522649-1831038044-1853292631-2271478464' # TrustedInstaller
    )
    if ($ownerSid -notin $trustedOwners) {
        throw "qOS executable uninstall path has an untrusted owner: $Path"
    }

    $trustedWriters = @($trustedOwners) + @('S-1-3-0') # CREATOR OWNER
    $writeMask = (
        [int][Security.AccessControl.FileSystemRights]::WriteData -bor
        [int][Security.AccessControl.FileSystemRights]::AppendData -bor
        [int][Security.AccessControl.FileSystemRights]::WriteExtendedAttributes -bor
        [int][Security.AccessControl.FileSystemRights]::DeleteSubdirectoriesAndFiles -bor
        [int][Security.AccessControl.FileSystemRights]::WriteAttributes -bor
        [int][Security.AccessControl.FileSystemRights]::Delete -bor
        [int][Security.AccessControl.FileSystemRights]::ChangePermissions -bor
        [int][Security.AccessControl.FileSystemRights]::TakeOwnership
    )
    foreach ($rule in @($acl.GetAccessRules($true, $true, [Security.Principal.SecurityIdentifier]))) {
        if (
            $rule.AccessControlType -eq [Security.AccessControl.AccessControlType]::Allow -and
            (([int]$rule.FileSystemRights -band $writeMask) -ne 0) -and
            $rule.IdentityReference.Value -notin $trustedWriters
        ) {
            throw "qOS executable uninstall path grants write access to an untrusted principal: $Path"
        }
    }
    return $item
}

function Assert-qOSTrustedPathChain {
    param(
        [Parameter(Mandatory = $true)][string]$Root,
        [Parameter(Mandatory = $true)][string]$Path,
        [switch]$Leaf
    )

    if (-not (Test-qOSPathInsideRoot -Path $Path -Root $Root)) {
        throw 'qOS executable uninstall path escapes its bound install root'
    }
    $rootItem = Assert-qOSTrustedPathAcl -Path $Root
    if (-not $rootItem.PSIsContainer) { throw 'qOS executable uninstall root is not a directory' }

    $relative = $Path.Substring($Root.Length).TrimStart('\')
    $parts = @($relative.Split([char[]]@('\'), [StringSplitOptions]::RemoveEmptyEntries))
    if ($parts.Count -eq 0) { throw 'qOS executable uninstall path cannot equal its install root' }
    $current = $Root
    $lastDirectoryPart = if ($Leaf) { $parts.Count - 2 } else { $parts.Count - 1 }
    for ($index = 0; $index -le $lastDirectoryPart; $index++) {
        $current = Join-Path $current $parts[$index]
        $directory = Assert-qOSTrustedPathAcl -Path $current
        if (-not $directory.PSIsContainer) { throw 'qOS executable uninstall path contains a non-directory component' }
    }
    if ($Leaf) {
        $leafItem = Assert-qOSTrustedPathAcl -Path $Path
        if ($leafItem.PSIsContainer) { throw 'qOS executable uninstall target is not a regular file' }
        return $leafItem
    }
}

function Get-qOSValidatedExecutableUninstallContract {
    param([Parameter(Mandatory = $true)]$Contract)

    if ([string]$Contract.kind -ne 'exe') { throw 'qOS executable uninstall contract has the wrong kind' }
    $rawRoot = [string]$Contract.installRoot
    $rawApplication = [string]$Contract.applicationPath
    $rawExecutable = [string]$Contract.executablePath
    foreach ($rawPath in @($rawRoot, $rawApplication, $rawExecutable)) {
        if ([string]::IsNullOrWhiteSpace($rawPath) -or $rawPath -notmatch '^[A-Za-z]:\\' -or $rawPath.Substring(2).Contains(':')) {
            throw 'qOS executable uninstall contract contains a non-absolute or alternate-stream path'
        }
    }
    $root = [IO.Path]::GetFullPath($rawRoot).TrimEnd('\')
    $application = [IO.Path]::GetFullPath($rawApplication)
    $executable = [IO.Path]::GetFullPath($rawExecutable)
    if ([IO.Path]::GetExtension($executable) -ine '.exe') {
        throw 'qOS executable uninstaller is not an EXE'
    }

    $underProgramFiles = $false
    foreach ($basePath in @($env:ProgramFiles, ${env:ProgramFiles(x86)}) | Where-Object { -not [string]::IsNullOrWhiteSpace($_) } | Select-Object -Unique) {
        $programRoot = [IO.Path]::GetFullPath([string]$basePath).TrimEnd('\')
        if ($root.StartsWith(($programRoot + '\'), [StringComparison]::OrdinalIgnoreCase)) {
            $underProgramFiles = $true
            break
        }
    }
    if (-not $underProgramFiles) {
        throw 'qOS executable uninstall root is not a product-specific Program Files directory'
    }

    $null = Assert-qOSTrustedPathChain -Root $root -Path $application -Leaf
    $executableItem = Assert-qOSTrustedPathChain -Root $root -Path $executable -Leaf
    $expectedHash = ([string]$Contract.executableSha256).ToLowerInvariant()
    if ($expectedHash -notmatch '^[a-f0-9]{64}$') { throw 'qOS executable uninstall digest is invalid' }
    $actualHash = (Get-FileHash -LiteralPath $executable -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($actualHash -cne $expectedHash) { throw 'qOS executable uninstaller changed after discovery' }

    $arguments = @($Contract.arguments)
    if ($arguments.Count -gt 32) { throw 'qOS executable uninstaller has too many arguments' }
    $normalizedArguments = @()
    $totalArgumentChars = 0
    foreach ($argument in $arguments) {
        if ($null -eq $argument) { throw 'qOS executable uninstaller has a null argument' }
        $text = [string]$argument
        if ($text.Length -gt 4096 -or $text -match '[\x00-\x1f\x7f]') {
            throw 'qOS executable uninstaller argument is invalid or too long'
        }
        $totalArgumentChars += $text.Length
        if ($totalArgumentChars -gt 16384) { throw 'qOS executable uninstaller arguments exceed their aggregate bound' }
        $normalizedArguments += $text
    }

    return [PSCustomObject]@{
        kind = 'exe'
        installRoot = $root
        applicationPath = $application
        executablePath = $executable
        executableSha256 = $expectedHash
        arguments = @($normalizedArguments)
    }
}

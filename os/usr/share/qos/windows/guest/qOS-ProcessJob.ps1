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

        private static void ThrowWin32(string message)
        {
            throw new Win32Exception(Marshal.GetLastWin32Error(), message);
        }
    }
}
'@ -Language CSharp
}

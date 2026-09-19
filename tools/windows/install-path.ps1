# Probe actual writes: directory handles can resolve to the public directory
# even when an MSIX process redirects individual files into its private cache.
function Assert-TiloPublicInstallDirectory {
    param([Parameter(Mandatory)][string]$Directory)

    if (-not ('Tilo.InstallPath' -as [type])) {
        Add-Type -TypeDefinition @'
using System;
using System.ComponentModel;
using System.Runtime.InteropServices;
using System.Text;
using Microsoft.Win32.SafeHandles;
namespace Tilo {
    public static class InstallPath {
        [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
        private static extern uint GetFinalPathNameByHandleW(
            SafeFileHandle handle, StringBuilder path, uint size, uint flags);
        public static string Resolve(SafeFileHandle handle) {
            var path = new StringBuilder(32768);
            uint length = GetFinalPathNameByHandleW(handle, path, (uint)path.Capacity, 0);
            if (length == 0) throw new Win32Exception(Marshal.GetLastWin32Error());
            if (length >= path.Capacity) throw new InvalidOperationException("Installation path is too long.");
            return path.ToString();
        }
    }
}
'@
    }
    $directoryPath = [IO.Path]::GetFullPath($Directory)
    [void][IO.Directory]::CreateDirectory($directoryPath)
    $probePath = Join-Path $directoryPath ('.tilo-install-probe-' + [guid]::NewGuid().ToString('N'))
    $probe = [IO.FileStream]::new($probePath, [IO.FileMode]::CreateNew,
        [IO.FileAccess]::ReadWrite, [IO.FileShare]::None, 1, [IO.FileOptions]::DeleteOnClose)
    try {
        $physicalPath = [Tilo.InstallPath]::Resolve($probe.SafeFileHandle)
        # Normal junctions are supported; reject the package-private cache
        # specifically, because Explorer cannot discover an installation there.
        if ($physicalPath -match '\\Packages\\[^\\]+\\LocalCache\\') {
            throw "Windows redirected installation writes into a private package cache: $physicalPath. Run tools/windows/install-apps.ps1 from an ordinary, unpackaged PowerShell session so Explorer can see the apps and Start shortcuts."
        }
    } finally {
        $probe.Dispose()
    }
}

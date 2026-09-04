#!/usr/bin/env bash
# Parse the native launcher when PowerShell is available; otherwise retain a
# strict static contract in the ordinary Linux invariant shard.
set -euo pipefail
repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
script="$repo/tools/Run-ImagerEphemeralRunner.ps1"

if command -v pwsh >/dev/null 2>&1; then
  pwsh -NoLogo -NoProfile -NonInteractive -Command \
    '$tokens=$null; $errors=$null; [void][Management.Automation.Language.Parser]::ParseFile($args[0],[ref]$tokens,[ref]$errors); if ($errors.Count) { $errors | Out-String | Write-Error; exit 1 }' \
    "$script"
fi

has() { grep -Fq -- "$2" "$1"; }
line_of() { grep -nF -- "$2" "$1" | head -n 1 | cut -d: -f1; }
safe_line="$(line_of "$script" '$SafeMetadata = @($UuidItem.FullName, $TokenItem.FullName)')"
try_line="$(line_of "$script" 'try {')"
url_line="$(line_of "$script" 'if ($Server -notin @("https://code.podesta.ai", "https://code.podesta.ai/"))')"
runner_line="$(line_of "$script" '$TrustedRunner = Assert-qOSTrustedRunner $Runner')"
start_line="$(line_of "$script" '$Process = Start-Process')"
finally_line="$(line_of "$script" '} finally {')"
remove_line="$(line_of "$script" 'Remove-Item -LiteralPath $Path -Force')"
finally_kill_line="$(grep -nF -- 'taskkill.exe" /PID $Process.Id /T /F' "$script" | tail -n 1 | cut -d: -f1)"
has "$script" 'Assert-qOSPrivateMetadataFile $UuidFile' \
  && has "$script" 'Assert-qOSPrivateMetadataFile $TokenFile' \
  && has "$script" '$SafeMetadata = @($UuidItem.FullName, $TokenItem.FullName)' \
  && has "$script" 'if ($Server -notin @("https://code.podesta.ai", "https://code.podesta.ai/"))' \
  && has "$script" '$Label -cne "windows"' \
  && has "$script" '"one-job", "--url"' \
  && has "$script" '"--token-url", $TokenUrl, "--label", "windows:host", "--wait"' \
  && has "$script" 'if (-not $Process.WaitForExit($MaxWait * 1000))' \
  && has "$script" 'taskkill.exe" /PID $Process.Id /T /F' \
  && has "$script" 'if (-not $Process.HasExited) { $Process.Kill() }' \
  && has "$script" '} finally {' \
  && has "$script" 'Remove-Item -LiteralPath $Path -Force' \
  && has "$script" 'if (Test-Path -LiteralPath $Path) { $CleanupFailures += $Path }' \
  && has "$script" "'^[0-9a-fA-F]{40}$'" \
  && has "$script" '$ParentAcl.AreAccessRulesProtected' \
  && [ "$safe_line" -lt "$try_line" ] \
  && [ "$try_line" -lt "$url_line" ] \
  && [ "$url_line" -lt "$runner_line" ] \
  && [ "$runner_line" -lt "$start_line" ] \
  && [ "$start_line" -lt "$finally_line" ] \
  && [ "$finally_line" -lt "$finally_kill_line" ] \
  && [ "$finally_kill_line" -lt "$remove_line" ] \
  && [ "$finally_line" -lt "$remove_line" ] \
  && ! grep -Eq 'Invoke-Expression|iex[[:space:]]' "$script" \
  || { printf 'Windows ephemeral runner launcher lost its fail-closed contract\n' >&2; exit 1; }

printf 'ok    Windows native runner has a parsed/static one-job cleanup contract\n'

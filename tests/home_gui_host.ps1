# A GUI "AI app" for home_test.py: a WinForms window that spawns the server (like VS Code, Claude Desktop or
# the Codex app do), runs a small task and writes what it saw to a result file.
# Usage: powershell -STA -File home_gui_host.ps1 <result.json> <server exe> <window title>
param([string]$Out, [string]$Exe, [string]$Title)
Add-Type -AssemblyName System.Windows.Forms
Add-Type @"
using System; using System.Text; using System.Runtime.InteropServices;
public static class W32 {
  [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetWindowText(IntPtr h, StringBuilder s, int n);
}
"@
$form = New-Object Windows.Forms.Form
$form.Text = $Title; $form.Width = 420; $form.Height = 220; $form.StartPosition = 'Manual'; $form.Location = '1000,80'
$form.Show(); $form.Activate()

function Pump($ms) { $end = [DateTime]::Now.AddMilliseconds($ms); while ([DateTime]::Now -lt $end) { [Windows.Forms.Application]::DoEvents(); Start-Sleep -Milliseconds 20 } }
function FgTitle { $sb = New-Object Text.StringBuilder 512; [void][W32]::GetWindowText([W32]::GetForegroundWindow(), $sb, 512); $sb.ToString() }

$psi = New-Object Diagnostics.ProcessStartInfo
$psi.FileName = $Exe; $psi.UseShellExecute = $false; $psi.RedirectStandardInput = $true; $psi.RedirectStandardOutput = $true; $psi.CreateNoWindow = $true
$p = [Diagnostics.Process]::Start($psi)
$script:n = 0
function Call($name, $arguments) {
  $script:n++
  $meta = @{ 'io.modelcontextprotocol/protocolVersion' = '2026-07-28'; 'io.modelcontextprotocol/clientCapabilities' = @{} }
  $msg = @{ jsonrpc = '2.0'; id = $script:n; method = 'tools/call'; params = @{ name = $name; arguments = $arguments; _meta = $meta } } | ConvertTo-Json -Depth 8 -Compress
  $p.StandardInput.WriteLine($msg); $p.StandardInput.Flush()
  $deadline = [DateTime]::Now.AddSeconds(40)
  while ([DateTime]::Now -lt $deadline) {
    $task = $p.StandardOutput.ReadLineAsync()
    while (-not $task.IsCompleted -and [DateTime]::Now -lt $deadline) { [Windows.Forms.Application]::DoEvents(); Start-Sleep -Milliseconds 15 }
    if (-not $task.IsCompleted) { break }
    $obj = $task.Result | ConvertFrom-Json
    if ($obj.id -eq $script:n) { return (($obj.result.content | Where-Object { $_.type -eq 'text' } | ForEach-Object { $_.text }) -join "`n") }
  }
  return 'TIMEOUT'
}

$res = @{}
try {
  Pump 1500
  $res.apps = Call 'list_apps' @{}
  $res.focus = Call 'focus_window' @{ name = 'DC-TARGET'; observe = 'none' }
  $res.fg_after_focus = FgTitle
  $res.finish = Call 'finish' @{ summary = 'gui host test' }
  Pump 800
  $res.fg_after_finish = FgTitle
  $res.form_handle = [int64]$form.Handle
} catch { $res.error = $_.ToString() }
$res | ConvertTo-Json -Depth 4 | Set-Content -Path $Out -Encoding UTF8
try { $p.StandardInput.Close(); if (-not $p.WaitForExit(4000)) { $p.Kill() } } catch {}
$form.Close()

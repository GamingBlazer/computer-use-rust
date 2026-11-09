# Compiles a tiny WinForms program named keepassxc.exe (one of the forbidden password-manager
# exe names) into $Dir and runs it, so the forbidden-app guard can be tested end to end on a window
# the test created itself. Nothing in it touches a real password manager.
param([string]$Dir = "$env:TEMP\dc_forbidden_test")
New-Item -ItemType Directory -Force -Path $Dir | Out-Null
$exe = Join-Path $Dir 'keepassxc.exe'
$src = @'
using System;
using System.Windows.Forms;
public class P {
  [STAThread] public static void Main() {
    var f = new Form(); f.Text = "DC Forbidden Test Window"; f.Width = 500; f.Height = 300;
    var t = new TextBox(); t.Dock = DockStyle.Fill; t.Multiline = true; t.Text = "secret vault text";
    f.Controls.Add(t); Application.Run(f);
  }
}
'@
Add-Type -TypeDefinition $src -ReferencedAssemblies System.Windows.Forms -OutputAssembly $exe -OutputType WindowsApplication
& $exe

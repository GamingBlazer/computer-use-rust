# Compiles a tiny WinForms program under a chosen exe name into $Dir (once) and prints its path. The
# security tests start it with arguments:  <exe> <title> <text> [password|slowpaste] [left] [top] [button caption]
# It shows one big text box (a password box when asked) and a button that appends " CLICKED" to the title,
# so a test can see whether a control was operated. Mode "slowpaste" shows instead a custom control that UI Automation
# cannot read and whose Ctrl+V handler keeps the thread busy for 2.5 s BEFORE it reads the clipboard (a deliberately slow
# target for the clipboard-restore test); the title then shows what it received as "[line1|line2]". Nothing in it touches a real password manager: a name such
# as keepassxc.exe only stands in for the forbidden-app list.
param([string]$Dir, [string]$Exe)
New-Item -ItemType Directory -Force -Path $Dir | Out-Null
$path = Join-Path $Dir $Exe
if (-not (Test-Path $path)) {
$src = @'
using System;
using System.Drawing;
using System.Windows.Forms;
public class SlowBox : Control {
  public string Buf = "";
  Form owner; string baseTitle;
  public SlowBox(Form f) { owner = f; baseTitle = f.Text; SetStyle(ControlStyles.Selectable | ControlStyles.UserPaint, true); TabStop = true; BackColor = Color.White; }
  protected override bool IsInputKey(Keys k) { return true; }
  protected override void OnKeyDown(KeyEventArgs e) {
    if (e.Control && e.KeyCode == Keys.V) {
      System.Threading.Thread.Sleep(2500);
      Buf += Clipboard.ContainsText() ? Clipboard.GetText() : "";
    } else if (e.KeyCode == Keys.Enter) { Buf += "|"; }
    else { base.OnKeyDown(e); return; }
    owner.Text = baseTitle + " [" + Buf + "]";
  }
  protected override void OnPaint(PaintEventArgs e) { e.Graphics.Clear(Color.White); e.Graphics.DrawString(Buf, Font, Brushes.Black, 4, 4); }
}
public class P {
  [STAThread] public static void Main(string[] a) {
    var f = new Form();
    f.Text = a.Length > 0 ? a[0] : "DC Dummy";
    f.Width = 560; f.Height = 230;
    f.StartPosition = FormStartPosition.Manual;
    int left = 120, top = 30;
    if (a.Length > 4) { int.TryParse(a[3], out left); int.TryParse(a[4], out top); }
    f.Left = left; f.Top = top;
    var t = new TextBox(); t.Name = "main"; t.Font = new Font("Segoe UI", 20);
    Control slow = null;
    if (a.Length > 2 && a[2] == "slowpaste") {
      slow = new SlowBox(f); slow.Dock = DockStyle.Fill; slow.Font = new Font("Segoe UI", 20);
    } else if (a.Length > 2 && a[2] == "password") {
      t.UseSystemPasswordChar = true; t.Dock = DockStyle.Top;
    } else {
      t.Multiline = true; t.Dock = DockStyle.Fill; t.Text = a.Length > 1 ? a[1] : "";
    }
    var b = new Button(); b.Text = a.Length > 5 ? a[5] : "DummyButton"; b.Name = "dummybutton"; b.Dock = DockStyle.Bottom; b.Height = 40;
    b.Click += (s, e) => { f.Text = f.Text + " CLICKED"; };
    if (slow != null) { f.Controls.Add(slow); f.Controls.Add(b); f.ActiveControl = slow; } else { f.Controls.Add(t); f.Controls.Add(b); }
    Application.Run(f);
  }
}
'@
Add-Type -TypeDefinition $src -ReferencedAssemblies System.Windows.Forms, System.Drawing -OutputAssembly $path -OutputType WindowsApplication
}
Write-Output $path

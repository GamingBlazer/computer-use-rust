# A plain white borderless full-screen window ("DC Cover Form"): evidence screenshots of the
# cursor halo then show only this test's own content, nothing from the user's other windows.
Add-Type -AssemblyName System.Windows.Forms
$f = New-Object Windows.Forms.Form
$f.Text = 'DC Cover Form'
$f.FormBorderStyle = 'None'
$f.BackColor = [Drawing.Color]::White
$f.StartPosition = 'Manual'
$f.Bounds = [Windows.Forms.Screen]::PrimaryScreen.Bounds
$f.TopMost = $true
$l = New-Object Windows.Forms.Label
$l.Text = 'desktop-control cursor halo test: a white screen, nothing else'
$l.AutoSize = $true; $l.Font = New-Object Drawing.Font('Segoe UI', 18); $l.Location = '40,40'
$f.Controls.Add($l)
[void]$f.ShowDialog()

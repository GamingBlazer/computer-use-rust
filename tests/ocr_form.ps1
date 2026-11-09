# WinForms test window whose text is painted with GDI (DrawString) on a plain panel: UI Automation
# cannot see it, so only OCR can find "Launch Rocket" / "Abort Mission" / "Confirm" (twice).
# One real Button ("Real Button") is visible to UI Automation. Clicks write a line to $env:DC_MARK.
param([string]$Title = 'DC OCR Test Form')
Add-Type -AssemblyName System.Windows.Forms, System.Drawing
$mark = $env:DC_MARK
$f = New-Object Windows.Forms.Form
$f.Text = $Title; $f.Width = 760; $f.Height = 560; $f.StartPosition = 'CenterScreen'
$p = New-Object Windows.Forms.Panel
$p.Dock = 'Fill'
$state = @{ status = 'idle' }
$rects = @{
  rocket  = New-Object Drawing.Rectangle(50, 40, 260, 64)
  abort   = New-Object Drawing.Rectangle(400, 40, 260, 64)
  confirm1 = New-Object Drawing.Rectangle(50, 330, 200, 56)
  confirm2 = New-Object Drawing.Rectangle(400, 330, 200, 56)
}
$font = New-Object Drawing.Font('Segoe UI', 16)
$p.Add_Paint({
  param($s, $e)
  $g = $e.Graphics
  $g.TextRenderingHint = 'ClearTypeGridFit'
  $g.Clear([Drawing.Color]::White)
  foreach ($k in 'rocket', 'abort', 'confirm1', 'confirm2') {
    $g.FillRectangle([Drawing.Brushes]::SteelBlue, $rects[$k])
  }
  $g.DrawString('Launch Rocket', $font, [Drawing.Brushes]::White, 66, 56)
  $g.DrawString('Abort Mission', $font, [Drawing.Brushes]::White, 416, 56)
  $g.DrawString('Confirm', $font, [Drawing.Brushes]::White, 66, 342)
  $g.DrawString('Confirm', $font, [Drawing.Brushes]::White, 416, 342)
  $g.DrawString('Status: ' + $state.status, $font, [Drawing.Brushes]::Black, 50, 160)
  $g.DrawString('The quick brown fox jumps over the lazy dog', $font, [Drawing.Brushes]::Black, 50, 230)
})
$p.Add_MouseClick({
  param($s, $e)
  foreach ($k in 'rocket', 'abort', 'confirm1', 'confirm2') {
    if ($rects[$k].Contains($e.Location)) {
      $state.status = "$k clicked"
      if ($mark) { Add-Content -Path $mark -Value $k }
      $p.Invalidate()
    }
  }
})
$b = New-Object Windows.Forms.Button
$b.Text = 'Real Button'; $b.Dock = 'Bottom'; $b.Height = 50
$b.Add_Click({ if ($mark) { Add-Content -Path $mark -Value 'real' } })
$f.Controls.Add($p)
$f.Controls.Add($b)
[void]$f.ShowDialog()

# Writes a PNG of the given size filled with realistic UI-sized text (for `desktop-control.exe ocr-bench`).
param([int]$Width = 2560, [int]$Height = 1440, [string]$Out = "$env:TEMP\dc_ocr_$($Width)x$($Height).png")
Add-Type -AssemblyName System.Drawing
$bmp = New-Object Drawing.Bitmap($Width, $Height)
$g = [Drawing.Graphics]::FromImage($bmp)
$g.Clear([Drawing.Color]::White)
$g.TextRenderingHint = 'ClearTypeGridFit'
$font = New-Object Drawing.Font('Segoe UI', 11)
$title = New-Object Drawing.Font('Segoe UI', 20, [Drawing.FontStyle]::Bold)
$g.DrawString('Quarterly Report Dashboard - OCR Benchmark Needle Text', $title, [Drawing.Brushes]::Black, 40, 20)
$words = 'invoice','customer','payment','overdue','balance','project','schedule','meeting','review','budget','forecast','delivery','account','settings','profile','network','status','pending','approved','archive'
$rand = New-Object Random(42)
$y = 80
while ($y -lt $Height - 30) {
  $x = 40
  while ($x -lt $Width - 600) {
    $line = (1..6 | ForEach-Object { $words[$rand.Next($words.Count)] }) -join ' '
    $g.DrawString($line, $font, [Drawing.Brushes]::Black, $x, $y)
    $x += 560
  }
  $y += 26
}
$g.Dispose()
$bmp.Save($Out, [Drawing.Imaging.ImageFormat]::Png)
$bmp.Dispose()
Write-Output $Out

$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing
# Monochrome Windows icon with an outlined tile and a white bypass arrow.
function RoundRect($x, $y, $w, $h, $r) {
    $p = New-Object System.Drawing.Drawing2D.GraphicsPath
    $p.AddArc($x, $y, $r, $r, 180, 90)
    $p.AddArc($x + $w - $r, $y, $r, $r, 270, 90)
    $p.AddArc($x + $w - $r, $y + $h - $r, $r, $r, 0, 90)
    $p.AddArc($x, $y + $h - $r, $r, $r, 90, 90)
    $p.CloseFigure()
    return $p
}
function Draw([int]$s) {
    $bmp = New-Object System.Drawing.Bitmap $s, $s
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
    $g.Clear([System.Drawing.Color]::Transparent)
    $k = $s / 256.0
    $bg = RoundRect (14 * $k) (14 * $k) (228 * $k) (228 * $k) (92 * $k)
    $tile = New-Object System.Drawing.SolidBrush ([System.Drawing.Color]::FromArgb(255, 16, 16, 16))
    $outline = New-Object System.Drawing.Pen ([System.Drawing.Color]::FromArgb(255, 221, 221, 221)), (14 * $k)
    $g.FillPath($tile, $bg)
    $g.DrawPath($outline, $bg)

    $white = [System.Drawing.Color]::White
    # barrier
    $bar = New-Object System.Drawing.Pen ([System.Drawing.Color]::FromArgb(150, 255, 255, 255)), (20 * $k)
    $bar.StartCap = 'Round'; $bar.EndCap = 'Round'
    $g.DrawLine($bar, (128 * $k), (148 * $k), (128 * $k), (190 * $k))
    # arc over the barrier
    $pen = New-Object System.Drawing.Pen $white, (22 * $k)
    $pen.StartCap = 'Round'; $pen.EndCap = 'Round'
    $g.DrawArc($pen, (64 * $k), (72 * $k), (128 * $k), (128 * $k), 180, 180)
    $g.DrawLine($pen, (64 * $k), (136 * $k), (64 * $k), (178 * $k))
    # arrow head at the right end, pointing down
    $g.DrawLine($pen, (192 * $k), (136 * $k), (192 * $k), (178 * $k))
    $g.DrawLine($pen, (170 * $k), (158 * $k), (192 * $k), (180 * $k))
    $g.DrawLine($pen, (214 * $k), (158 * $k), (192 * $k), (180 * $k))
    $pen.Dispose()
    $bar.Dispose()
    $outline.Dispose()
    $tile.Dispose()
    $bg.Dispose()
    $g.Dispose()
    $ms = New-Object System.IO.MemoryStream
    $bmp.Save($ms, [System.Drawing.Imaging.ImageFormat]::Png)
    $bmp.Dispose()
    $png = $ms.ToArray()
    $ms.Dispose()
    return ,$png
}
$sizes = 16, 24, 32, 48, 256
$imgs = $sizes | ForEach-Object { , (Draw $_) }
$out = New-Object System.IO.MemoryStream
$bw = New-Object System.IO.BinaryWriter $out
$bw.Write([uint16]0); $bw.Write([uint16]1); $bw.Write([uint16]$sizes.Count)
$offset = 6 + 16 * $sizes.Count
for ($i = 0; $i -lt $sizes.Count; $i++) {
    $s = $sizes[$i]; $d = $imgs[$i]
    $b = if ($s -ge 256) { 0 } else { $s }
    $bw.Write([byte]$b); $bw.Write([byte]$b); $bw.Write([byte]0); $bw.Write([byte]0)
    $bw.Write([uint16]1); $bw.Write([uint16]32)
    $bw.Write([uint32]$d.Length); $bw.Write([uint32]$offset)
    $offset += $d.Length
}
foreach ($d in $imgs) { $bw.Write($d) }
[System.IO.File]::WriteAllBytes("$PSScriptRoot\icon.ico", $out.ToArray())
[System.IO.File]::WriteAllBytes("$PSScriptRoot\logo_preview.png", $imgs[4])
[System.IO.File]::WriteAllBytes("$PSScriptRoot\logo_preview_32.png", $imgs[2])

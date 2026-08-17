param(
    [string]$Src,
    [int]$X0, [int]$X1, [int]$Y0, [int]$Y1,
    # Background luminance ceiling: pixels brighter than this count as ink.
    [int]$Threshold = 60
)
Add-Type -AssemblyName System.Drawing
$bmp = New-Object System.Drawing.Bitmap($Src)
$minY = -1; $maxY = -1; $minX = -1; $maxX = -1
for ($y = $Y0; $y -le $Y1; $y++) {
    for ($x = $X0; $x -le $X1; $x++) {
        $c = $bmp.GetPixel($x, $y)
        $lum = [int](0.299 * $c.R + 0.587 * $c.G + 0.114 * $c.B)
        if ($lum -gt $Threshold) {
            if ($minY -lt 0) { $minY = $y }
            $maxY = $y
            if ($minX -lt 0 -or $x -lt $minX) { $minX = $x }
            if ($x -gt $maxX) { $maxX = $x }
        }
    }
}
$bmp.Dispose()
Write-Output "ink x=[$minX,$maxX] y=[$minY,$maxY] in probe x=[$X0,$X1] y=[$Y0,$Y1]"

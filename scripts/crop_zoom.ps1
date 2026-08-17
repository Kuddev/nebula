param(
    [string]$Src,
    [string]$Out,
    [int]$X, [int]$Y, [int]$W, [int]$H,
    [int]$Scale = 2
)
Add-Type -AssemblyName System.Drawing
$img = [System.Drawing.Image]::FromFile($Src)
$rect = New-Object System.Drawing.Rectangle($X, $Y, $W, $H)
$bmp = New-Object System.Drawing.Bitmap([int]($W * $Scale), [int]($H * $Scale))
$g = [System.Drawing.Graphics]::FromImage($bmp)
$g.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::NearestNeighbor
$dest = New-Object System.Drawing.Rectangle(0, 0, $bmp.Width, $bmp.Height)
$g.DrawImage($img, $dest, $rect, [System.Drawing.GraphicsUnit]::Pixel)
$bmp.Save($Out)
$g.Dispose(); $bmp.Dispose(); $img.Dispose()
Write-Output "saved $Out"

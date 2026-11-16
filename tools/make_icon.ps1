# Regenerates fb.ico (Windows icon resource) from fb.png.
# Requires only .NET System.Drawing, which ships with Windows PowerShell.
param(
    [string]$Png = (Join-Path $PSScriptRoot '..\fb.png'),
    [string]$Ico = (Join-Path $PSScriptRoot '..\fb.ico')
)

Add-Type -AssemblyName System.Drawing

$source = [System.Drawing.Image]::FromFile($Png)
$sizes = @(16, 24, 32, 48, 64, 128, 256)
$images = @()

foreach ($size in $sizes) {
    $bitmap = New-Object System.Drawing.Bitmap($size, $size)
    $graphics = [System.Drawing.Graphics]::FromImage($bitmap)
    $graphics.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
    $graphics.PixelOffsetMode = [System.Drawing.Drawing2D.PixelOffsetMode]::HighQuality
    $graphics.DrawImage($source, 0, 0, $size, $size)
    $graphics.Dispose()

    $stream = New-Object System.IO.MemoryStream
    $bitmap.Save($stream, [System.Drawing.Imaging.ImageFormat]::Png)
    $images += ,([byte[]]$stream.ToArray())
    $stream.Dispose()
    $bitmap.Dispose()
}
$source.Dispose()

$stream = New-Object System.IO.MemoryStream
$writer = New-Object System.IO.BinaryWriter($stream)
$writer.Write([uint16]0)          # reserved
$writer.Write([uint16]1)          # type: icon
$writer.Write([uint16]$images.Count)

$offset = 6 + 16 * $images.Count
for ($i = 0; $i -lt $images.Count; $i++) {
    $size = $sizes[$i]
    $data = $images[$i]
    $writer.Write([byte]$(if ($size -eq 256) { 0 } else { $size }))
    $writer.Write([byte]$(if ($size -eq 256) { 0 } else { $size }))
    $writer.Write([byte]0)        # palette
    $writer.Write([byte]0)        # reserved
    $writer.Write([uint16]1)      # planes
    $writer.Write([uint16]32)     # bits per pixel
    $writer.Write([uint32]$data.Length)
    $writer.Write([uint32]$offset)
    $offset += $data.Length
}
foreach ($data in $images) {
    $writer.Write($data)
}
$writer.Flush()

[System.IO.File]::WriteAllBytes($Ico, $stream.ToArray())
$stream.Dispose()
"wrote $Ico ($((Get-Item $Ico).Length) bytes, $($sizes.Count) sizes)"

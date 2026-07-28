$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing

$pluginRoot = Join-Path $PSScriptRoot '..\stream-deck\com.2sbsi.audio-switch.sdPlugin'
foreach ($scale in 1, 2) {
    $suffix = if ($scale -eq 1) { '' } else { '@2x' }
    $outputPath = Join-Path $pluginRoot "imgs\plugin$suffix.png"
    $bitmap = [System.Drawing.Bitmap]::new(144 * $scale, 144 * $scale)
    $graphics = [System.Drawing.Graphics]::FromImage($bitmap)
    $graphics.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
    $graphics.Clear([System.Drawing.Color]::FromArgb(20, 25, 34))

    $pen = [System.Drawing.Pen]::new([System.Drawing.Color]::FromArgb(99, 216, 255), 10 * $scale)
    $pen.StartCap = [System.Drawing.Drawing2D.LineCap]::Round
    $pen.EndCap = [System.Drawing.Drawing2D.LineCap]::Round
    $graphics.DrawLine($pen, 32 * $scale, 52 * $scale, 82 * $scale, 52 * $scale)
    $graphics.DrawLine($pen, 70 * $scale, 41 * $scale, 82 * $scale, 52 * $scale)
    $graphics.DrawLine($pen, 70 * $scale, 63 * $scale, 82 * $scale, 52 * $scale)
    $graphics.DrawLine($pen, 112 * $scale, 92 * $scale, 62 * $scale, 92 * $scale)
    $graphics.DrawLine($pen, 74 * $scale, 81 * $scale, 62 * $scale, 92 * $scale)
    $graphics.DrawLine($pen, 74 * $scale, 103 * $scale, 62 * $scale, 92 * $scale)

    $dot = [System.Drawing.SolidBrush]::new([System.Drawing.Color]::FromArgb(247, 249, 252))
    $graphics.FillEllipse($dot, 22 * $scale, 82 * $scale, 20 * $scale, 20 * $scale)
    $graphics.FillEllipse($dot, 102 * $scale, 42 * $scale, 20 * $scale, 20 * $scale)
    $bitmap.Save($outputPath, [System.Drawing.Imaging.ImageFormat]::Png)

    $dot.Dispose()
    $pen.Dispose()
    $graphics.Dispose()
    $bitmap.Dispose()
}

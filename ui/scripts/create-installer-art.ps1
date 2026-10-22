Add-Type -AssemblyName System.Drawing

$outDir = Join-Path $PSScriptRoot '..\windows'

function New-Art($width, $height, $filename, $draw) {
    $bitmap = [System.Drawing.Bitmap]::new($width, $height, [System.Drawing.Imaging.PixelFormat]::Format24bppRgb)
    $graphics = [System.Drawing.Graphics]::FromImage($bitmap)
    $graphics.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
    $graphics.TextRenderingHint = [System.Drawing.Text.TextRenderingHint]::AntiAliasGridFit
    try {
        & $draw $graphics $width $height
        $bitmap.Save((Join-Path $outDir $filename), [System.Drawing.Imaging.ImageFormat]::Bmp)
    } finally {
        $graphics.Dispose()
        $bitmap.Dispose()
    }
}

function Color($hex) { [System.Drawing.ColorTranslator]::FromHtml($hex) }

New-Art 164 314 'installer-sidebar.bmp' {
    param($g, $w, $h)
    $bg = [System.Drawing.Drawing2D.LinearGradientBrush]::new(
        [System.Drawing.Rectangle]::new(0, 0, $w, $h), (Color '#101217'), (Color '#182b49'), 72)
    $g.FillRectangle($bg, 0, 0, $w, $h)
    $bg.Dispose()

    $glow = [System.Drawing.SolidBrush]::new((Color '#123b77'))
    $g.FillEllipse($glow, -80, 118, 250, 210)
    $glow.Dispose()

    $line = [System.Drawing.Pen]::new((Color '#2979d7'), 2)
    $line2 = [System.Drawing.Pen]::new((Color '#92c7ff'), 2)
    $g.DrawRectangle($line, 30, 67, 84, 55)
    $g.DrawRectangle($line2, 50, 85, 84, 55)
    $g.DrawLine($line, 67, 122, 67, 132)
    $g.DrawLine($line, 53, 132, 82, 132)
    $g.DrawLine($line2, 91, 140, 91, 150)
    $g.DrawLine($line2, 77, 150, 106, 150)
    $line.Dispose()
    $line2.Dispose()

    $mark = [System.Drawing.SolidBrush]::new((Color '#56a6ff'))
    $g.FillEllipse($mark, 72, 98, 8, 8)
    $g.FillEllipse($mark, 113, 111, 8, 8)
    $mark.Dispose()

    $white = [System.Drawing.SolidBrush]::new((Color '#f5f8ff'))
    $muted = [System.Drawing.SolidBrush]::new((Color '#a9bdd5'))
    $titleFont = [System.Drawing.Font]::new('Segoe UI', 17, [System.Drawing.FontStyle]::Bold)
    $smallFont = [System.Drawing.Font]::new('Segoe UI', 8, [System.Drawing.FontStyle]::Regular)
    $g.DrawString('NerveDesk', $titleFont, $white, 16, 192)
    $g.DrawString('CONNECT YOUR DESK', $smallFont, $muted, 17, 227)
    $titleFont.Dispose()
    $smallFont.Dispose()
    $white.Dispose()
    $muted.Dispose()

    $accent = [System.Drawing.SolidBrush]::new((Color '#2979d7'))
    $g.FillRectangle($accent, 16, 269, 28, 3)
    $accent.Dispose()
}

New-Art 150 57 'installer-header.bmp' {
    param($g, $w, $h)
    $bg = [System.Drawing.Drawing2D.LinearGradientBrush]::new(
        [System.Drawing.Rectangle]::new(0, 0, $w, $h), (Color '#141821'), (Color '#153763'), 0)
    $g.FillRectangle($bg, 0, 0, $w, $h)
    $bg.Dispose()
    $blue = [System.Drawing.SolidBrush]::new((Color '#3185e6'))
    $white = [System.Drawing.SolidBrush]::new((Color '#f5f8ff'))
    $letter = [System.Drawing.Font]::new('Segoe UI', 14, [System.Drawing.FontStyle]::Bold)
    $name = [System.Drawing.Font]::new('Segoe UI', 11, [System.Drawing.FontStyle]::Bold)
    $g.FillRectangle($blue, 11, 13, 31, 31)
    $g.DrawString('N', $letter, $white, 17, 15)
    $g.DrawString('NerveDesk', $name, $white, 49, 18)
    $letter.Dispose()
    $name.Dispose()
    $blue.Dispose()
    $white.Dispose()
}

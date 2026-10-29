param([switch]$SkipBuild)
$ErrorActionPreference = 'Stop'
$projectRoot = Split-Path -Parent $PSScriptRoot
Push-Location $projectRoot
try {
    if (-not $SkipBuild) {
        npm ci --prefix ui --no-audit --no-fund --prefer-offline
        if ($LASTEXITCODE -ne 0) { throw 'Frontend dependency installation failed' }
        npm run build --prefix ui
        if ($LASTEXITCODE -ne 0) { throw 'Frontend build failed' }
        cargo build --release --locked
        if ($LASTEXITCODE -ne 0) { throw 'Release build failed' }
    }
    $packageDirectory = Join-Path $projectRoot ('target/package-' + [guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Path $packageDirectory,dist -Force | Out-Null
    Copy-Item -LiteralPath target/release/TgWsProxyPlus.exe -Destination "$packageDirectory/TgWsProxyPlus.exe"
    Copy-Item -LiteralPath LICENSE,LICENSE.upstream,THIRD_PARTY_NOTICES.txt,README.md,CONTRIBUTING.md -Destination $packageDirectory
    Copy-Item -LiteralPath docs -Destination $packageDirectory -Recurse
    New-Item -ItemType Directory -Path "$packageDirectory/assets" -Force | Out-Null
    Copy-Item -LiteralPath assets/brand,assets/screenshots -Destination "$packageDirectory/assets" -Recurse
    $binary = Get-Item -LiteralPath "$packageDirectory/TgWsProxyPlus.exe"
    if ($binary.Length -gt 2MB) { throw 'Windows EXE exceeds the 2 MiB size budget' }
    $digest = (Get-FileHash -LiteralPath $binary.FullName -Algorithm SHA256).Hash.ToLowerInvariant()
    "$digest  TgWsProxyPlus.exe" | Set-Content -LiteralPath "$packageDirectory/SHA256SUMS.txt" -Encoding ascii
    Copy-Item -LiteralPath $binary.FullName -Destination dist/TgWsProxyPlus-windows-x86_64.exe
    Compress-Archive -Path "$packageDirectory/*" -DestinationPath dist/TgWsProxyPlus-windows-x86_64.zip -Force
    Get-FileHash -LiteralPath dist/TgWsProxyPlus-windows-x86_64.exe,dist/TgWsProxyPlus-windows-x86_64.zip -Algorithm SHA256 |
        ForEach-Object { "$($_.Hash.ToLowerInvariant())  $([System.IO.Path]::GetFileName($_.Path))" } |
        Set-Content -LiteralPath dist/SHA256SUMS.txt -Encoding ascii
    Write-Output "EXE: $($binary.Length) bytes; SHA256: $digest"
    $resolvedPackage = (Resolve-Path -LiteralPath $packageDirectory).Path
    $targetRoot = [System.IO.Path]::GetFullPath((Join-Path $projectRoot 'target'))
    if (-not $resolvedPackage.StartsWith($targetRoot + [System.IO.Path]::DirectorySeparatorChar, [System.StringComparison]::OrdinalIgnoreCase)) { throw 'Unexpected package directory' }
    Remove-Item -LiteralPath $resolvedPackage -Recurse -Force
} finally {
    Pop-Location
}

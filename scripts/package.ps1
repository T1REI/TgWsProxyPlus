$ErrorActionPreference = 'Stop'
$projectRoot = Split-Path -Parent $PSScriptRoot
Push-Location $projectRoot
try {
    cargo build --release --locked
    if ($LASTEXITCODE -ne 0) { throw 'Release build failed' }
    $packageDirectory = Join-Path $projectRoot ('target/package-' + [guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Path $packageDirectory,dist -Force | Out-Null
    Copy-Item -LiteralPath target/release/TgWsProxyPlus.exe -Destination "$packageDirectory/TgWsProxyPlus.exe"
    Copy-Item -LiteralPath LICENSE,LICENSE.upstream,THIRD_PARTY_NOTICES.txt,README.md -Destination $packageDirectory
    Copy-Item -LiteralPath docs -Destination $packageDirectory -Recurse
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
} finally {
    Pop-Location
}

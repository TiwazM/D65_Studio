param([string]$OutputDirectory = (Join-Path $PSScriptRoot 'dist'), [switch]$TestWebClient)
$ErrorActionPreference = 'Stop'
Push-Location $PSScriptRoot
try {
    cargo test --locked
    if ($LASTEXITCODE -ne 0) { throw 'Rust tests failed' }
    if ($TestWebClient) {
        cargo test --locked --test windows_mount -- --ignored
        if ($LASTEXITCODE -ne 0) { throw 'Native WebClient test failed' }
    }
    cargo build --locked --release --target x86_64-pc-windows-msvc
    if ($LASTEXITCODE -ne 0) { throw 'Windows release build failed' }
    $releaseName = 'MEGA65-DiskLab-0.3.1-Windows-x64-r2'
    $releasePath = Join-Path $OutputDirectory $releaseName
    if (Test-Path -LiteralPath $releasePath) { throw "Output already exists: $releasePath. Choose a fresh output directory." }
    New-Item -ItemType Directory -Path $releasePath -Force | Out-Null
    Copy-Item -LiteralPath 'target/x86_64-pc-windows-msvc/release/mega65-disklab.exe' -Destination $releasePath
    foreach ($file in @('WINDOWS.txt', 'README.md', 'FORMAT.md', 'LICENSE')) { Copy-Item -LiteralPath $file -Destination $releasePath }
    New-Item -ItemType Directory -Path (Join-Path $releasePath 'launcher'), (Join-Path $releasePath 'Samples'), (Join-Path $releasePath 'verification') | Out-Null
    foreach ($file in @('Resident-driver.txt', 'LOAD-compatibility.txt')) { Copy-Item -LiteralPath (Join-Path 'launcher' $file) -Destination (Join-Path $releasePath 'launcher') }
    foreach ($file in @('TEST-D65.d65', 'TEST-D81.d81', 'HELLO.prg', 'HOOKBAS.D65', 'HOOKTEST.D65', 'RESDRV.D65', 'RESMAX.D65', 'RESCAP.D65')) { Copy-Item -LiteralPath (Join-Path 'Images' $file) -Destination (Join-Path $releasePath 'Samples') }
    if (Test-Path 'verification/windows') { Copy-Item -LiteralPath 'verification/windows' -Destination (Join-Path $releasePath 'verification') -Recurse }
    foreach ($folder in @('load-hook', 'resident-driver', 'sd-export')) { Copy-Item -LiteralPath (Join-Path 'verification' $folder) -Destination (Join-Path $releasePath 'verification') -Recurse }
    $metadataText = cargo metadata --locked --offline --format-version 1 --filter-platform x86_64-pc-windows-msvc
    if ($LASTEXITCODE -ne 0) { throw 'Dependency inventory failed' }
    $metadata = $metadataText | ConvertFrom-Json
    $resolvedIds = @{}
    foreach ($node in $metadata.resolve.nodes) { $resolvedIds[$node.id] = $true }
    $licenseRoot = Join-Path $releasePath 'Third-party-licenses'
    New-Item -ItemType Directory -Path $licenseRoot | Out-Null
    $inventory = @('Third-party dependencies for the Windows target. See each package folder for notices.', '')
    foreach ($package in $metadata.packages) {
        if (!$resolvedIds.ContainsKey($package.id) -or !$package.source) { continue }
        $name = "$($package.name)-$($package.version)"
        $inventory += "$name : $($package.license)"
        $packageRoot = Split-Path -Parent $package.manifest_path
        $destination = Join-Path $licenseRoot $name
        New-Item -ItemType Directory -Path $destination | Out-Null
        $notices = Get-ChildItem -LiteralPath $packageRoot -Recurse -File | Where-Object { $_.Name -match '^(LICENSE|LICENCE|COPYING|NOTICE|AUTHORS|OFL)([._-]|$)' }
        foreach ($notice in $notices) {
            $relative = $notice.FullName.Substring($packageRoot.Length).TrimStart('\', '/')
            $out = Join-Path $destination $relative
            New-Item -ItemType Directory -Path (Split-Path -Parent $out) -Force | Out-Null
            Copy-Item -LiteralPath $notice.FullName -Destination $out
        }
        if (!$notices) {
            # Some crate packages state their license only in README/Cargo.toml.
            Copy-Item -LiteralPath $package.manifest_path -Destination $destination
            Get-ChildItem -LiteralPath $packageRoot -File -Filter 'README*' | Copy-Item -Destination $destination
            $standard = if ($package.license -match 'Apache-2.0') { 'Apache-2.0' } elseif ($package.license -eq 'BSL-1.0') { 'BSL-1.0' } elseif ($package.license -eq 'CC0-1.0') { 'CC0-1.0' } else { throw "Missing license text for $name" }
            Copy-Item -LiteralPath "packaging/license-texts/$standard.txt" -Destination (Join-Path $destination 'LICENSE.txt')
        }
    }
    $inventory | Set-Content -LiteralPath (Join-Path $licenseRoot 'INDEX.txt') -Encoding UTF8
    $hash = (Get-FileHash -Algorithm SHA256 -LiteralPath (Join-Path $releasePath 'mega65-disklab.exe')).Hash.ToLowerInvariant()
    "$hash  mega65-disklab.exe" | Set-Content -LiteralPath (Join-Path $releasePath 'SHA256SUMS.txt') -Encoding ASCII
    Get-ChildItem -LiteralPath $releasePath -Recurse -File | Where-Object { $_.LastWriteTime.Year -lt 1980 } | ForEach-Object { $_.LastWriteTime = [datetime]'1980-01-01' }
    Compress-Archive -LiteralPath $releasePath -DestinationPath (Join-Path $OutputDirectory "$releaseName.zip") -CompressionLevel Optimal
    Write-Output (Join-Path $OutputDirectory "$releaseName.zip")
} finally { Pop-Location }

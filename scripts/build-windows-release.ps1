# Build a runnable Windows release folder (mousehop.exe + GTK DLLs).
# Matches README "Windows" §3–4 and the CI windows-release-build packaging:
#   1. Session GTK env (Path, PKG_CONFIG, PKG_CONFIG_PATH, Lib)
#   2. Confirm gtk4 + libadwaita via pkgconf
#   3. cargo build -p mousehop --release
#   4. Stage exe + GTK DLLs + gdk-pixbuf SVG loader under target/release/mousehop-windows/
#
# One-time prereqs (gvsbuild GTK stack) are documented in README — not run here.
# Override the GTK prefix with $env:MOUSEHOP_GTK_ROOT if needed.
#
# Run from the repo root, or via the workspace "Build mousehop.exe (release)" task.

$ErrorActionPreference = "Stop"

$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

$gtkRoot = if ($env:MOUSEHOP_GTK_ROOT) {
    $env:MOUSEHOP_GTK_ROOT
} else {
    "C:\gtk-build\gtk\x64\release"
}
$gtkBin = Join-Path $gtkRoot "bin"
$gtkLib = Join-Path $gtkRoot "lib"
$pkgConfig = Join-Path $gtkBin "pkgconf.exe"
$pkgConfigPath = Join-Path $gtkLib "pkgconfig"
$gtkPc = Join-Path $pkgConfigPath "gtk4.pc"
$adwPc = Join-Path $pkgConfigPath "libadwaita-1.pc"

if (-not (Test-Path $pkgConfig)) {
    Write-Error "pkgconf not found at $pkgConfig. Build the gvsbuild GTK stack first (see README Windows section), or set MOUSEHOP_GTK_ROOT."
}
if (-not (Test-Path $gtkPc)) {
    Write-Error "Missing $gtkPc. Build gtk4 with gvsbuild (see README Windows section)."
}
if (-not (Test-Path $adwPc)) {
    Write-Error "Missing $adwPc. Build libadwaita with gvsbuild from outside this repo, e.g.:`n  Set-Location C:\gtk-build`n  gvsbuild build --vs-ver vs2026 --msys-dir=C:\msys64 --configuration release --no-deps --fast-build libadwaita"
}

# README §3 session env
$env:Path = "$gtkBin;$env:Path"
$env:PKG_CONFIG = $pkgConfig
$env:PKG_CONFIG_PATH = $pkgConfigPath
if ($env:Lib) {
    if ($env:Lib -notlike "*gtk-build*" -and $env:Lib -notlike "*$gtkLib*") {
        $env:Lib = "$gtkLib;$env:Lib"
    }
} else {
    $env:Lib = $gtkLib
}

Write-Host "==> GTK root: $gtkRoot"
$adwVer = & $pkgConfig --modversion libadwaita-1
if ($LASTEXITCODE -ne 0) {
    Write-Error "pkgconf failed for libadwaita-1"
}
$gtkVer = & $pkgConfig --modversion gtk4
if ($LASTEXITCODE -ne 0) {
    Write-Error "pkgconf failed for gtk4"
}
Write-Host "==> gtk4 $gtkVer / libadwaita $adwVer"
Write-Host "==> PKG_CONFIG=$env:PKG_CONFIG"
Write-Host "==> PKG_CONFIG_PATH=$env:PKG_CONFIG_PATH"
Write-Host "==> Lib=$env:Lib"

$exe = Join-Path $root "target\release\mousehop.exe"
$exeFull = [System.IO.Path]::GetFullPath($exe)
# Stage like CI: exe + GTK DLLs so the folder runs without relying on PATH
$stage = Join-Path $root "target\release\mousehop-windows"
$stageFull = [System.IO.Path]::GetFullPath($stage)

# Running mousehop locks the release binary (cargo can't replace it) and/or
# staged DLLs. Stop those processes before build + restage.
function Stop-MousehopLockingPaths {
    param(
        [string[]]$PathPrefixes
    )
    $locked = Get-CimInstance Win32_Process -Filter "Name = 'mousehop.exe'" -ErrorAction SilentlyContinue |
        Where-Object {
            if (-not $_.ExecutablePath) { return $false }
            $path = [System.IO.Path]::GetFullPath($_.ExecutablePath)
            foreach ($prefix in $PathPrefixes) {
                if ($path.Equals($prefix, [System.StringComparison]::OrdinalIgnoreCase) -or
                    $path.StartsWith($prefix.TrimEnd('\') + '\', [System.StringComparison]::OrdinalIgnoreCase)) {
                    return $true
                }
            }
            $false
        }
    if (-not $locked) {
        return
    }
    $pids = @($locked | ForEach-Object { $_.ProcessId })
    Write-Host "==> stopping $($pids.Count) mousehop process(es): $($pids -join ', ')"
    foreach ($proc in $locked) {
        Stop-Process -Id $proc.ProcessId -Force -ErrorAction Stop
    }
    Start-Sleep -Milliseconds 400
}

Stop-MousehopLockingPaths -PathPrefixes @($exeFull, $stageFull)

Write-Host "==> cargo build -p mousehop --release"
cargo build -p mousehop --release
if ($LASTEXITCODE -ne 0) {
    exit $LASTEXITCODE
}

if (-not (Test-Path $exe)) {
    Write-Error "Expected binary missing: $exe"
}

# In case something relaunched during the build, clear locks again before restage.
Stop-MousehopLockingPaths -PathPrefixes @($exeFull, $stageFull)

if (Test-Path $stage) {
    Remove-Item -Recurse -Force $stage
}
# gdk-pixbuf loads SVG only through this module. bin\*.dll includes
# rsvg-2-*.dll but not the loader; without it the tray glyph is blank.
$pixbufLoaderRel = "lib\gdk-pixbuf-2.0\2.10.0\loaders\pixbufloader_svg.dll"
$pixbufLoaderSrc = Join-Path $gtkRoot $pixbufLoaderRel
if (-not (Test-Path $pixbufLoaderSrc)) {
    Write-Error "Missing SVG pixbuf loader at $pixbufLoaderSrc. Build librsvg with gvsbuild (see README Windows section), e.g.:`n  Set-Location C:\gtk-build`n  gvsbuild build --vs-ver vs2026 --msys-dir=C:\msys64 --configuration release gtk4 libadwaita librsvg"
}

New-Item -ItemType Directory -Path $stage | Out-Null
Copy-Item -Path $exe -Destination $stage
Get-ChildItem -Path (Join-Path $gtkBin "*.dll") -File -Recurse |
    Copy-Item -Destination $stage

# Keep the conventional lib\gdk-pixbuf-… tree (runtime looks here) and
# also place a copy next to the exe so LoadLibrary dependency search is
# the same as for the other GTK DLLs.
$pixbufLoaderDstDir = Join-Path $stage "lib\gdk-pixbuf-2.0\2.10.0\loaders"
New-Item -ItemType Directory -Path $pixbufLoaderDstDir -Force | Out-Null
Copy-Item -Path $pixbufLoaderSrc -Destination $pixbufLoaderDstDir
Copy-Item -Path $pixbufLoaderSrc -Destination $stage

# Absolute paths with forward slashes (gdk-pixbuf-query-loaders style).
# mousehop rewrites a temp cache at startup if the folder is relocated.
$loaderAbs = [System.IO.Path]::GetFullPath((Join-Path $pixbufLoaderDstDir "pixbufloader_svg.dll"))
$loaderCacheEntry = $loaderAbs.Replace('\', '/')
$loadersCache = @(
    '# GdkPixbuf Image Loader Modules file',
    '# Generated by scripts/build-windows-release.ps1 for the staged Mousehop folder.',
    '#',
    "`"$loaderCacheEntry`"",
    '"svg" 6 "gdk-pixbuf" "Scalable Vector Graphics" "LGPL"',
    '"image/svg+xml" "image/svg" "image/svg-xml" "image/vnd.adobe.svg+xml" "text/xml-svg" "image/svg+xml-compressed" ""',
    '"svg" "svgz" "svg.gz" ""',
    '" <svg" "*    " 100',
    '" <!DOCTYPE svg" "*             " 100',
    ''
) -join "`n"
$loadersCachePath = Join-Path $stage "lib\gdk-pixbuf-2.0\2.10.0\loaders.cache"
Set-Content -Path $loadersCachePath -Value $loadersCache -Encoding ascii -NoNewline

Write-Host "==> done: $exe"
Write-Host "==> staged: $stage\mousehop.exe (+ GTK DLLs + SVG pixbuf loader)"

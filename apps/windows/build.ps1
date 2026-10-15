# Builds Agent Burn for Windows from this checkout: the engine (agent-burn.exe)
# and the application (agent-burn-windows.exe), from the same revision, with
# the locked Cargo files and the C runtime linked in, so they run on a clean
# Windows without the Visual C++ Redistributable. Then packs them, with the
# licenses and the instructions, in a versioned ZIP and its SHA-256.
#
# Usage: pwsh apps/windows/build.ps1 [-Check] [-OutDir <folder>]
#   -Check   also runs rustfmt, clippy and the tests of both projects.
#   -OutDir  where the folder, the ZIP and its checksum go (apps/windows/dist).
param(
    [switch] $Check,
    [string] $OutDir
)
$ErrorActionPreference = "Stop"
$PSNativeCommandUseErrorActionPreference = $true

$app = $PSScriptRoot
$root = Resolve-Path (Join-Path $app "../..")
$engine = Join-Path $root "rust"
$target = "x86_64-pc-windows-msvc"
if (-not $OutDir) { $OutDir = Join-Path $app "dist" }

function Invoke-Cargo {
    param([string] $Folder, [string[]] $Arguments)
    Push-Location $Folder
    try {
        & cargo @Arguments
        if ($LASTEXITCODE -ne 0) { throw "cargo $($Arguments -join ' ') failed in $Folder" }
    } finally { Pop-Location }
}

# Only the target is linked statically; build scripts keep the host runtime.
$env:RUSTFLAGS = "-C target-feature=+crt-static"

if ($Check) {
    Invoke-Cargo $app @("fmt", "--check")
    Invoke-Cargo $app @("clippy", "--locked", "--target", $target, "--all-targets", "--", "-D", "warnings")
    Invoke-Cargo $app @("test", "--locked", "--target", $target)
    Invoke-Cargo $engine @("test", "--workspace", "--locked", "--target", $target)
}

Invoke-Cargo $engine @("build", "--release", "--locked", "--target", $target, "-p", "agent-burn", "--bin", "agent-burn")
Invoke-Cargo $app @("build", "--release", "--locked", "--target", $target, "--bin", "agent-burn-windows")

$version = (Select-String -Path (Join-Path $app "Cargo.toml") -Pattern '^version = "(.+)"' |
    Select-Object -First 1).Matches[0].Groups[1].Value
$name = "agent-burn-windows-$version-x64"
$stage = Join-Path $OutDir $name
if (Test-Path $stage) { Remove-Item -Recurse -Force $stage }
New-Item -ItemType Directory -Force (Join-Path $stage "licenses") | Out-Null

Copy-Item (Join-Path $app "target/$target/release/agent-burn-windows.exe") $stage
Copy-Item (Join-Path $engine "target/$target/release/agent-burn.exe") $stage
Copy-Item (Join-Path $app "README.md") $stage
Copy-Item (Join-Path $root "apps/agent-burn/LICENSE") (Join-Path $stage "LICENSE.txt")
Copy-Item (Join-Path $app "NOTICE") (Join-Path $stage "NOTICE.txt")
Copy-Item (Join-Path $app "licenses/*") (Join-Path $stage "licenses")

# The crates linked into either executable, with their licenses.
$crates = foreach ($folder in @($app, $engine)) {
    Push-Location $folder
    try {
        & cargo tree --locked --target $target -e normal --prefix none --format "{p} ({l})" |
            ForEach-Object { $_ -replace " \(\*\)$", "" -replace " \([A-Z]:[^)]*\)", "" }
    } finally { Pop-Location }
}
@("Third-party crates linked into agent-burn-windows.exe and agent-burn.exe,",
  "with their licenses as declared on crates.io:", "") +
    ($crates | Where-Object { $_ } | Sort-Object -Unique) |
    Set-Content -Encoding utf8 (Join-Path $stage "licenses/THIRD-PARTY.txt")

$zip = Join-Path $OutDir "$name.zip"
if (Test-Path $zip) { Remove-Item -Force $zip }
Compress-Archive -Path $stage -DestinationPath $zip
$hash = (Get-FileHash -Algorithm SHA256 $zip).Hash.ToLowerInvariant()
"$hash  $name.zip" | Set-Content -Encoding ascii -NoNewline "$zip.sha256"

foreach ($exe in @("agent-burn-windows.exe", "agent-burn.exe")) {
    $runtime = Select-String -Path (Join-Path $stage $exe) -Pattern "VCRUNTIME140" -SimpleMatch -Quiet
    if ($runtime) { throw "$exe needs the Visual C++ runtime" }
}
& (Join-Path $stage "agent-burn.exe") --version
"$zip"
"sha256 $hash"

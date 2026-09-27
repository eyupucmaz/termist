# Installs termist on Windows, in PowerShell:
#   powershell -ExecutionPolicy Bypass -c "irm https://eyupucmaz.github.io/termist/install.ps1 | iex"
# It finds the newest release (pre-releases included) and runs that release's own
# installer, which checks the download's checksum and puts `termist` in ~/.cargo/bin.
$ErrorActionPreference = 'Stop'

# Windows PowerShell 5.1 may not offer TLS 1.2 on its own.
[Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12

$repo = 'eyupucmaz/termist'
$releases = Invoke-RestMethod -Uri "https://api.github.com/repos/$repo/releases?per_page=1"
$tag = ($releases | Select-Object -First 1).tag_name
if (-not $tag) {
    throw "termist: could not find a release of $repo"
}
Write-Host "termist: installing $tag"
Invoke-RestMethod -Uri "https://github.com/$repo/releases/download/$tag/termist-installer.ps1" | Invoke-Expression

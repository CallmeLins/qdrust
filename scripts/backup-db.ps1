param([Parameter(Mandatory=$true)][string]$Database,[Parameter(Mandatory=$true)][string]$Output)
$resolved = (Resolve-Path -LiteralPath $Database).Path
$parent = Split-Path -Parent (Resolve-Path -LiteralPath (Split-Path -Parent $Output) -ErrorAction SilentlyContinue)
if (-not $parent) { New-Item -ItemType Directory -Path (Split-Path -Parent $Output) -Force | Out-Null }
sqlite3 $resolved "VACUUM INTO '$Output';"
if ($LASTEXITCODE -ne 0) { throw "SQLite backup failed" }
Write-Output "Backup written to $Output"

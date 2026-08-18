param([Parameter(Mandatory=$true)][string]$Backup,[Parameter(Mandatory=$true)][string]$Database)
if (-not (Test-Path -LiteralPath $Backup -PathType Leaf)) { throw "Backup not found: $Backup" }
$targetDir = Split-Path -Parent $Database
if ($targetDir) { New-Item -ItemType Directory -Path $targetDir -Force | Out-Null }
Copy-Item -LiteralPath $Backup -Destination $Database -Force
Write-Output "Database restored to $Database"

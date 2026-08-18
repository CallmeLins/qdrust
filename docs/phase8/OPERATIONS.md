# SQLite Operations

Run these commands while the server is stopped, or against a consistent SQLite snapshot:

```powershell
pwsh -File scripts/backup-db.ps1 -Database data/qdrust.db -Output backups/qdrust-$(Get-Date -Format yyyyMMdd-HHmmss).db
pwsh -File scripts/restore-db.ps1 -Backup backups/qdrust-20260818-120000.db -Database data/qdrust.db
```

After restore, start the server and verify `/ready`. The migration runner is forward-only; take a backup before upgrading the image.

## Upgrade And Rollback

Use immutable semantic-version image tags. Before upgrading:

1. Stop writes and create a database backup.
2. Record the currently deployed image digest.
3. Pull the target version and start it with the same `/data` volume.
4. Verify `/health`, `/ready`, login and one read-only workflow.

If health checks fail, stop the new container, restore the pre-upgrade database backup, and start the recorded previous image digest. Database migrations are forward-only, so rolling back only the image without restoring the database is not supported.

Release tags matching `v*` publish `linux/amd64` and `linux/arm64` images to GHCR with provenance and SBOM attestations. CI rejects images with known fixed HIGH or CRITICAL vulnerabilities.

# Operations & Release

> 返回 [README](../README.md)

运行期维护（SQLite 备份 / 恢复）与发布门禁清单。升级与回滚的完整操作步骤见 [部署 · 更新](deployment.md#更新)。

## 数据库备份与恢复

Run these commands while the server is stopped, or against a consistent SQLite snapshot:

```powershell
pwsh -File scripts/backup-db.ps1 -Database data/qdrust.db -Output backups/qdrust-$(Get-Date -Format yyyyMMdd-HHmmss).db
pwsh -File scripts/restore-db.ps1 -Backup backups/qdrust-20260818-120000.db -Database data/qdrust.db
```

After restore, start the server and verify `/ready`. The migration runner is forward-only, so rolling back only the image without restoring the database is not supported — take a backup before upgrading the image.

Release tags matching `v*` publish `linux/amd64` and `linux/arm64` images to GHCR with provenance and SBOM attestations. CI rejects images with known fixed HIGH or CRITICAL vulnerabilities.

## 数据库迁移

Migrations live in `migrations/` (SQLite) and `migrations-mysql/` (MySQL), and sqlx records
a sha384 checksum of every file it applies in `_sqlx_migrations`. A file whose bytes change
after it has been applied makes that deployment refuse to start:

```
Error: migration 202609060001 was previously applied but has been modified
```

Nothing relaxes that check at runtime, so **a migration is frozen the moment a release tag
contains it**: change the schema with a new dated file, never by editing or deleting an
existing one. Comments are content too — a stale doc path in a migration header is not worth
breaking every upgrade, and a docs reshuffle that rewrites one in place is enough to do it
(202609060001 shipped in v0.1.10 through v0.1.12, so editing its header comment would have
broken all three).

For the same reason `.gitattributes` pins `*.sql` to `text eol=lf`: the working copy, a fresh
clone, and the Linux image build must hash identical bytes, and `core.autocrlf=true` would
otherwise materialise CRLF on Windows and give one migration two different checksums.

`python3 scripts/check-migrations.py` enforces this against the newest `v*` tag and runs in CI.
To repair a released migration that was edited by mistake, restore the bytes it shipped with
(`git checkout <tag> -- <path>`); rewriting the stored checksum instead only unblocks the one
machine that was patched and leaves every other deployment failing.

## 发布检查清单

### 本地门禁

- `cargo fmt --all -- --check`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test --workspace --all-targets`
- `python scripts/check-migrations.py`
- `npm --prefix webui ci`
- `npm --prefix webui run generate:api`
- `npm --prefix webui run lint`
- `npm --prefix webui run test`
- `npm --prefix webui run build`
- Build the Docker image and verify `/health`, `/ready`, and `/`.
- Back up the deployment database before replacing an existing image.

### 浏览器验收

- Initialize the first administrator and sign out/in.
- Import and edit a representative legacy QD HAR.
- Create a task, run it immediately, inspect steps, and cancel an active run.
- Publish and copy a public template.
- Create a note, plugin, Webhook channel, and notification action.
- Repeat the main workflow at desktop and mobile widths.
- Verify a second user cannot access the first user's resources.

### 发布门禁

- Bump the version with `python scripts/bump-version.py <x.y.z>` (updates the
  workspace version, the WebUI package, the OpenAPI document, and both
  lockfiles in one step) and commit before tagging.
- Push an immutable `vMAJOR.MINOR.PATCH` tag only after all gates pass.
- Confirm the GHCR manifest contains amd64 and arm64 images.
- Confirm provenance and SBOM attestations exist.
- Confirm Trivy reports no fixed HIGH or CRITICAL vulnerabilities.
- Install into an empty data volume, then exercise backup, upgrade, restore, and rollback.

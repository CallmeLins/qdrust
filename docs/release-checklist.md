# Release Checklist

## Local Gates

- `cargo fmt --all -- --check`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test --workspace --all-targets`
- `npm --prefix webui ci`
- `npm --prefix webui run generate:api`
- `npm --prefix webui run lint`
- `npm --prefix webui run test`
- `npm --prefix webui run build`
- Build the Docker image and verify `/health`, `/ready`, and `/`.
- Back up the deployment database before replacing an existing image.

## Browser Gates

- Initialize the first administrator and sign out/in.
- Import and edit a representative legacy QD HAR.
- Create a task, run it immediately, inspect steps, and cancel an active run.
- Publish and copy a public template.
- Create a note, plugin, Webhook channel, and notification action.
- Repeat the main workflow at desktop and mobile widths.
- Verify a second user cannot access the first user's resources.

## Release Gates

- Push an immutable `vMAJOR.MINOR.PATCH` tag only after all gates pass.
- Confirm the GHCR manifest contains amd64 and arm64 images.
- Confirm provenance and SBOM attestations exist.
- Confirm Trivy reports no fixed HIGH or CRITICAL vulnerabilities.
- Install into an empty data volume, then exercise backup, upgrade, restore, and rollback.

# QD HAR compatibility corpus

Place anonymized legacy QD `.har` files in this directory. The `qdrust-core` integration test automatically verifies every file without requiring a test-code change.

Fixtures must contain no credentials, cookies, personal data, or reachable production endpoints. Keep unusual QD extension fields: lossless preservation is part of the import contract.

---
name: ship-observer-release
description: After the user approves an observer change, commit it, bump Cargo.toml, tag a GitHub Release, push main and the tag, wait for CI, and reinstall. Use when the user says push, ship, land it, LGTM, release, or tag, or when an approved change is about to land on main.
---

# Ship observer release

Do this in the same turn as the approval. Do not stop after `git push origin main`.

1. Commit the approved change. Run `cargo test` first.
2. Read `version` in `Cargo.toml`. Bump the patch (`0.1.10` → `0.1.11`).
3. Sync `Cargo.lock` and the version string in `README.md`.
4. Commit `Release vX.Y.Z`.
5. Create an annotated tag `vX.Y.Z` on that commit.
6. Push both: `git push origin main` and `git push origin vX.Y.Z`.
7. Wait until the Release workflow on that tag succeeds and the GitHub Release exists.
8. Reinstall:

```bash
curl -fsSL https://raw.githubusercontent.com/khotan-core/harness-message-capture/main/dist/install.sh | bash
```

A push to `main` does not publish a binary. Only a `v*` tag runs `.github/workflows/release.yml`. Never leave `Cargo.toml` unbumped. Never leave `~/.local/bin/khotan-observer` on a stale build. `cargo build --release` is not a substitute unless the user asks for a local swap.

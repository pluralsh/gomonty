# Vendored crate patches

These crates replace upstream versions through `[patch]` entries in the root
`Cargo.toml`. They exist only to move off `zip` 0.6.x / 8.x, which are affected
by the zip "cloaking" parser-differential advisory (fixed in `zip` 9.0.5).
Upstream Monty 1.4.0 pins `ruff_db`/`ty_*` to exactly 0.0.14 and
`monty-typeshed` to `zip` 0.6.6, so the fix can't be picked up by a version bump.

| Crate | Source | Changes |
| --- | --- | --- |
| `ruff_db` 0.0.14 | crates.io (astral-sh/ruff `62914c4`) | `zip` `8.0` -> `9.0.5`; `src/vendored.rs` adapted to zip 9 (same change as astral-sh/ruff#29808) |
| `ty_vendored` 0.0.14 | crates.io (astral-sh/ruff `62914c4`) | `zip` `8.0` -> `9.0.5` |
| `monty-typeshed` 1.4.0 | pydantic/monty `4b015dc` (tag 1.4.0) | `zip` `0.6.6` -> `9.0.5`; `build.rs` uses `SimpleFileOptions`; workspace-inherited manifest fields made explicit |

The packaged `Cargo.lock` / `Cargo.toml.orig` files were removed; Cargo does
not read them for dependencies.

## Removing

When bumping the Monty pin, check whether upstream already uses `zip` >= 9.0.5
(`monty-typeshed`, and `ruff_db`/`ty_vendored` at the version Monty pins). If
it does, delete this directory and the `[patch]` entries, run `cargo update`,
and confirm with `cargo tree -i zip`. If upstream changes the pinned versions
but still uses an old `zip`, re-vendor the new versions with the same changes;
a `[patch]` whose version no longer matches is silently ignored (`cargo` prints
"patch ... was not used in the crate graph").

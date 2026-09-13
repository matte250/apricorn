# Renderer implementation draft — 2026-09-13

PR #2 contained planning only. This implementation is isolated from the
combined dirty worktree on `codex/ds-renderer-oracle`, based on upstream
`main` at `2c2fad21c7d2c0cb9a713ee4888650f9d92bf826`.

## Scope

- Fixed-point camera/model concatenation, homogeneous clipping, preserved
  triangle/quad geometry, scanline rasterization and native texture data.
- Light/material evaluation, Z/W, translucent IDs, shadow stencil,
  fog/edge/AA paths with synthetic tests; no whole-core exactness claim.
- Field lighting and raw player-texture integration only. The local NPC
  manager, script/game integration, persistent desktop saves and unrelated
  text changes are excluded. House captures therefore lack Mom's billboard;
  that is a separate field-object submission workstream, not an omitted
  raster primitive. The live-light clock still uses the existing local
  ticks/60 approximation and is not an exact RTC contract.
- Raw oracle color/depth/attribute plus geometry/register diagnostics,
  pair/sequence comparator, indoor/outdoor review capture and benchmark.
- Existing main field hashes are preserved, not replaced with unverified
  local hashes. The newly added motion hash list is a historical diagnostic,
  not an oracle-approved reference.

## Verification of this isolated branch

- `cargo check --workspace --all-targets`: passed.
- `cargo test -p apricorn-gfx --lib --test raster`: 31 + 31 passed.
- `cargo test -p apricorn-harness --lib --bin apricorn-gx-diff`: 65 + 2 passed.
- ROM-gated `field_hg`: two passed, one historical golden failed.
- ROM-gated `field_system_hg`: furniture review passed; four historical
  frame/sequence checks failed; release benchmark not run in this branch.
- Fresh isolated-branch bedroom captures reproduce the earlier raw results:
  29–63 color, 0–5 depth, 28–39 attribute differences per frame, zero coverage
  differences, against the historical local oracle files. These files still
  have incomplete capture provenance, as recorded in the renderer work cards;
  this is diagnostic evidence, not a completed parity gate.
- The previously reported 4.113 ms mean / 6.072 ms maximum benchmark belongs
  to the combined worktree, not this isolated branch.

Full GX command/matrix-stack execution, per-command material and texture
matrix state, DIRECT/COMP4x4, field ground shadows, and complete raw indoor/
outdoor oracle gates remain open. This is a reviewable checkpoint, not a
merge-ready completion of Phase 5B. No ROM, save, screenshots or generated
raw game assets are included in the commit.

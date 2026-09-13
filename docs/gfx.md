# Graphics pipeline — model, rasterizer, presenters

The Phase 3 architecture doc: how a tick becomes pixels, and why the
pipeline is shaped the way it is. The hardware study behind the layer
model is `docs/nds-2d.md` (which cites the vendored NitroSDK headers
and pret usage per fact); this document covers the software built on
it: `apricorn-core::frame` / `app` / `assets`, the `apricorn-gfx`
rasterizer, and the two presenters — the headless dump CLI and the
`apricorn-desktop` shell.

The Phase 3 exit criterion, met: the engine boots from the ROM and
shows the HGSS title screen in a window, with the same pixels the
committed golden hashes certify.

## Topology: desktop → gfx → core

Three crates, one direction of dependency:

```
apricorn-desktop ──→ apricorn-gfx ──→ apricorn-core
(apricorn-harness ──────────────────────↗)
```

* **`apricorn-core`** owns the *model*: the logical frame, input, the
  app contract, and ROM-direct asset loading. Headless, zero rendering
  dependencies, all-integer state. This is where the harness's
  comparison contract lives, so it must never depend on anything
  GPU-shaped.
* **`apricorn-gfx`** is the deterministic CPU rasterizer: a logical
  frame plus an asset source become two 256×192 RGBA8 buffers. Pure
  function, no floating point, no time, no state — the same frame
  hashes the same pixels everywhere, forever. It depends only on core,
  so CI rasterizes and hashes without any GPU.
* **`apricorn-desktop`** is window + present glue only: winit for the
  window and events, wgpu to upload and draw the two buffers. No game
  logic — the pacer decides *when* ticks happen, never *what* they
  compute.

The obvious alternative — model in `apricorn-gfx`, apps in
`apricorn-core` — creates a dependency cycle (core→gfx for types,
gfx→core for asset chunks). The chosen split keeps the future
`apricorn-android` presenter trivially swappable and lets the GPU
stay out of CI entirely (plan Phase 3 risk table).

## The pipeline, per tick

```
Input (keyboard → REG_KEYXY bits)
   │
   ▼
BootChain.tick(frame, input)        apricorn_core::app — deterministic
   │  produces
   ▼
LogicalFrame                       apricorn_core::frame — pure data
   │  rasterized by
   ▼
render(frame, store) → [ScreenBuffer; 2]   apricorn_gfx — engines A/B
   │  mapped through DisplaySelect by the presenter
   ▼
presenter (wgpu window)  or  apricorn-gfx-dump (PNG + SHA-1)
```

Every stage is a pure function of the frame index and the input
sequence. The wall clock appears exactly once, in the desktop pacer,
and only chooses *when* to advance the tick counter — no timing value
ever reaches engine state. That is what makes the golden-hash tests
meaningful: they hash pixels that the harness could reproduce
frame-index by frame-index.

## The logical frame model (`apricorn-core::frame`)

One `LogicalFrame` is the complete observable video state at one tick:
both engines' four BG layers (`BgLayer`), each engine's blend and
master-brightness units, the backdrop colors, and a `DisplaySelect`
saying which engine drives which LCD. No pixel data lives in the
frame — layers reference loaded assets through opaque `AssetId`
handles that the asset store resolves at raster time — so a frame is
plain integers end to end: comparable with `==`, hashable, and
serializable for the harness without touching a ROM.

The field geometry mirrors the hardware study: engines A (MAIN) and
B (SUB), four text BG layers each under `BGxCNT`, one blend unit
(`BLDCNT`/`BLDALPHA`) and one brightness unit per engine. The display
mapping models pret's `screensFlipped` idiom (`GfGfx_SwapDisplay`,
`src/gf_gfx_planes.c:86`) as `DisplaySelect::SubOnTop` — both Phase 3
apps set it, which is why the title logo (engine B content) appears
on the top LCD. The rasterizer is display-agnostic: it renders
engines A and B; the *caller* maps them to LCDs.

## Apps and their assets (`apricorn-core::app`, `::assets`)

An `App` is one scene: `tick` advances on a frame token, `frame`
returns the logical frame the last tick produced, `next` says when
the scene is done. `BootChain` strings scenes in boot order and
cycles after the last (the title's 2340-frame idle timeout returns to
the intro, as the game's own chain does). Each app module's doc
comment maps it to its pret source and lists deferrals.

### Loading: ROM-direct at runtime

`apricorn_core::assets::AssetStore` opens `hg_usa.nds` (SHA-1 pinned
to the known dump constant, as `apricorn-tools verify` does), resolves
a NitroFS path + NARC member, LZ10-decompresses when the magic is
`0x10`, parses with the format decoders, and converts via
`cache::encode_*` *in memory* — the identical code path to the
converter, so there is no cache-format skew. The on-disk cache stays
a tooling artifact; the runtime never reads it.

### The copyright beat (`app::intro_copyright`)

Port of pret `src/intro_movie_scene_1.c` through `WAIT_GAMEFREAK`.
Engine A BG0 (copyright text) scrolls Y 128→0 at the Game Freak logo;
engine B BG1 carries the GF logo, BG0 the blank cover sharing BG1's
char block. Timings: 30-frame hold, 60-frame two-engine alpha fade
(`ev = counter*31/60`, exact integers), 20-frame gap, GF logo,
110-frame hold, skip on A/START/touch after the logo.

Assets (NARC `a/2/6/2`, table in `assets::copyright_beat` with
member-by-member citations):

| Layer | Char (NCGR) | Screen (NSCR) | Notes |
|---|---|---|---|
| MAIN BG0 | 5 (LZ) | 13 | 4bpp; palette `1.pal` |
| SUB BG1 | 4 (LZ) | 12 (LZ) | 4bpp; palette `0.pal` |
| SUB BG0 | — (shares BG1's) | 14 (LZ) | blank cover |

Palettes load 0x140 bytes (160 colors) into each engine's BG slot 0 —
pinned by the ROM test as observationally full for this beat. The
scene's later-appearing layers (sunrise art, members 6/7 and
15–18) are in the table for fidelity but never shown within
copyright-beat scope.

### The title screen (`app::title_screen`)

Port of the statics in pret `src/title_screen.c`: engine B (SUB)
carries all visible art; engine A's BG0 is the deferred Ho-Oh 3D
model (renders as the absent/black layer it is), BG3 is the runtime
"TOUCH TO START" window (cleared window + the 45-frame flash cycle's
frame-indexed state; the text itself is Phase 4). The logo (BG2)
fades in via y-scroll `t/2` and alpha blend ramping `t→31`; the flash
cycle is 45 frames; the idle timeout at frame 2340 returns to the
intro — an honest loop, not a freeze.

Assets (NARC `a/0/4/6`, uncompressed, table in `assets::title_screen`):

| Layer | Char (NCGR) | Screen (NSCR) | Notes |
|---|---|---|---|
| SUB BG1 | 15 | 17 | 4bpp static art |
| SUB BG2 | 3 | 0 | 8bpp — the game logo |
| SUB BG3 | 34 | 35 | 8bpp — version art |

Palettes: `4.pal` → SUB BG slot 0, `13.pal` → MAIN BG slot 0, full
0x200-byte loads. pret also loads `4.pal` into the SUB *extended*
palette RAM for the logo; extended palettes are deferred
(`docs/nds-2d.md`) and with ext mode off the logo reads the regular
load — which is what the model carries.

## The rasterizer (`apricorn-gfx`)

The intro renderer also draws the ROM's Ethan/Lyra selection portraits,
touch-advance button, and Marill using the resource tables in
`oaks_speech_obj.c` and `resdat` members 24–27/78. OBJ uses separate
palettes, signed cell offsets, 1D/2D tile addressing, flips, and the
intro's axis-aligned NANR transforms. OAM order resolves sprite overlaps;
OBJ wins a BG priority tie. Plane brightness and alpha blending apply
before master brightness. General rotated sprites, mosaic, the yes/no
cursor and sprite-completion timing
remain deferred.

Gender selection updates SUB BG palette words 12–15 with the original
red outline and sine-driven fill brightness. The frame carries mutable
BGR555 palette overrides separately from immutable ROM palette loads.

Window pixel index zero is transparent regardless of the color stored
at that palette-bank entry. Window tilemap writes still replace the old
map on their own layer. Menu borders use nine tiles; dialogue borders
use `render_window.s`'s eighteen-tile layout (two tiles left, three right).
Down arrows overwrite that border while preserving its palette bank.
The dialogue-frame loader also reproduces `sub_0200EA68`: the three
arrow poses from frame archive member 22 are shifted left three pixels
and blitted over border tiles 10/11 with index zero as the color key.
The printer addresses these composed tiles at the frame's base plus 18.
Holding a paragraph pause and then advancing is covered by a ROM test
that checks all four animation steps and verifies the border is restored.

`tests/oak_hg.rs` replays the tutorial, dialogue, Marill, and gender menu.
Set `APRICORN_RENDER_OUT` to an ignored output directory when running it
to save PNGs for visual review. No ROM pixels are committed.

`render(frame, store) -> [ScreenBuffer; 2]`: per engine, screen-entry
decode, tile fetch, scroll wrap per size (256/512 masks),
priority compositing (tie → lower BG index; pixel 0 transparent),
4bpp bank lookup vs 8bpp full-palette lookup, backdrop color, the
plane-mask alpha blend pass (`(first·EVA + second·EBV + 8) >> 4`,
EVA/EBV clamped to 16 — pinned by tests), master brightness last.
No floating point anywhere; all formulas are the integer ones pret or
the SDK uses.

Verified two ways: synthetic-fixture unit tests (hand-computed
pixels for flips, palette indexing, scroll wrap, priority ordering,
blend math, brightness — no ROM needed in CI) and the ROM-dependent
golden hashes below.

**A channel-order bug shipped and was caught here.** `cache.rs`'s
BGR555→RGBA decoder had R and B swapped — it read the format name's
msb-first channel order as a bit order, while the rasterizer's own
backdrop decode (and GBATEK, and the SDK's `GX_RGB` macro) had it
right. Every ROM-derived palette was channel-swapped; the title logo
rendered with yellow and blue exchanged. The unit fixture shared the
wrong belief, so it passed; grayscale art (the copyright screens) was
invariant under the swap, so those golden hashes survived unchanged.
Found by eyeball in the desktop window (2026-09-07), fixed in the
decoder, and all hashes regenerated. The lesson is baked into the
test layout: the golden hashes are the end-to-end pin, but they need
human eyes on real color at least once per pipeline change — which is
what the demo script and the window are for.

## Field (3D) layer

`apricorn-gfx::field` draws the overworld: the picture engine A's BG0
shows while a map is up. The production path now projects and
rasterizes through `gx3d`: signed fixed-point matrices, the DS viewport
divider, integer screen vertices, scanline spans, 24-bit depth and
fixed-point perspective attributes. The SDK sine table is generated
as a build-time constant; no floating-point operation occurs while a
field frame is rendered.

Opaque silhouettes use the GX left/right edge-ownership rules instead
of filling both sides of every span. Outdoor maps add one narrowly
scoped safety pass after opaque land cells and before props: an
alpha-zero pixel is sealed only when the original land buffer covers
both horizontal neighbours or both vertical neighbours. The pass reads
a snapshot, so it cannot grow a silhouette or bridge a gap wider than
one pixel. This removes the black one-pixel cell cracks exposed by the
transparent 3D clear plane without reintroducing bright roof and room
outline texels.

### What the original does

The field binds BG0 to the 3D core (`GX_BG0_AS_3D`,
`src/field/fieldmap.c:514`), turns the plane on once the map is loaded
(`:749`), and draws each frame in `ov01_021E6220` (`:580-613`): reset,
push the camera (`Camera_PushLookAtToNNSGlb`), draw the loaded map
cells and the props, then — with the projection's `_32` biased — the
field effects and the billboard lists, restore the projection, draw the
3D object tasks, and swap with `GX_SORTMODE_AUTO` under the camera's
Z-buffer mode. The hardware draws opaque polygons first and translucent
ones after them, depth tested but (with the depth-update bit clear)
not written; the clear colour is black with alpha 0
(`G3X_SetClearColor(RGB_BLACK, 0, …)`, `src/gf_3d_vramman.c:60`), so
uncovered pixels are transparent to the 2D layers below.

### Compositing rules (`raster.rs`)

When an engine frame carries a `field`, BG0 *is* the 3D plane:

* `field::render` runs first and yields RGBA8 (alpha 0 where nothing
  covered the pixel, the polygon's alpha otherwise) plus a depth
  buffer. The compositor samples that in BG0's place, honouring BG0's
  `priority`, `enabled` and `hidden_rect` (the hardware window) and
  ignoring BG0's tilemap, windows and edits — a tilemap cannot show on
  a BG bound to the 3D core.
* Everything else is unchanged: BG1–3 and OBJ composite by priority
  (ties to the lower BG index; OBJ wins ties), then the blend unit,
  the backdrop, and master brightness last.
* A 3D pixel blends with the topmost second-target pixel below it by
  its **own** alpha — `(first · (a+1) + second · (32−a−1) + 16) >> 5`
  per channel, rounded to nearest by the `+ 16` (half the divisor),
  melonDS `GPU2D_Soft.h` `ColorBlend5` — whenever `BLDCNT` names that
  plane a second target, regardless of the effect mode or the
  first-target mask; an opaque pixel (`a = 31`) is unchanged, so the
  register `EVA`/`EBV` never apply to the 3D plane. Without a second
  target the brightness fades (effects 2/3) apply to it as to any
  first-target plane. (melonDS works in 6-bit channels; this pipeline
  keeps its 8-bit convention.)
* The game shows the field's BG0 at priority 1 below the priority-0
  text layer (`sBgTemplate_3`) and above BG1/BG2 at 3 — the scene
  that binds a field must enable BG0 and set that priority, as the
  ROM tests do (`GfGfx_EngineATogglePlanes(GX_PLANEMASK_BG0, 1)` at
  `fieldmap.c:749`; `gf_3d_render.c:48` is the simple manager's
  `G2_SetBG0Priority(1)`).

### Camera math (`field/camera.rs`)

Port of `src/camera.c` and the preset table `ov01_02206478`
(`asm/overlay_01_021EABA8.s:465`, 17 rows of `{fx32 distance; u16
angle x, y, z, pad; u16 perspectiveType; u16 fovy; fx32 near; fx32
far; VecFx32 lookAtOffset}`, read by `FieldCamera_Create`). Preset 0
(outdoor) is perspective at distance ≈ 667, pitch `0xDD62`, half-angle
`0x5C1`; preset 4 (rooms, the bedroom) is orthographic at ≈ 1563.5,
pitch `0xDC82`, half-angle `0x281`; both near 150, far 1200 / 1736.

* **Angles and the table.** Angles are 16-bit units; sines and cosines
  come from the SDK's `FX_SinCosTable_` (`fx_trig.h:20-26`, index
  `angle >> 4`, 4096 fx16 pairs) — regenerated as `round(sin · 4096)`,
  which reproduces the vendored table exactly (SHA-1 pinned in the
  unit tests). The table is not unit-length under rounding (the pitch
  pair for preset 0 has length 4093.3), so the camera ends 666.48
  from its target rather than the preset's 666.92 — as on the DS.
* **Position.** `Camera_CalcLookAtPosFromTargetAndAngle`
  (`camera.c:20-26`): `pos = target + (sin(ay)·d·cos(ax), sin(−ax)·d,
  cos(ay)·d·cos(ax))` with `FX_Mul`'s `(a·b + 0x800) >> 12` rounding at
  each step, then the preset's look-at offset on both. The target is
  the player's position vector (tile centre, `x·16+8`, ground height,
  `z·16+8`).
* **View.** `MTX_LookAt(pos, (0,1,0), target)` — gluLookAt in the
  SDK's row-vector form, camera space +x right, +y up, looking down −z.
* **Projection.** Perspective: `MTX_PerspectiveW(fovySin, fovyCos,
  aspect, n, f)` = gluPerspective with `cot = fovyCos / fovySin` — the
  preset's `perspectiveAngle` is the vertical FOV's *half* angle.
  Orthographic (`Camera_ApplyPerspectiveType`, `camera.c:271-274`):
  `y = FX_Mul(FX_Div(fovySin, fovyCos), distance)`, `x = FX_Mul(y,
  aspect)`, `MTX_OrthoW(y, −y, −x, x, n, f)` — the extents are computed
  in fx32 exactly as the C does (95.8125 × 127.7422 for preset 4).
  `FX_Mul` is `(a·b + 0x800) >> 12`; `FX_Div` runs the hardware
  divider in 64/32 mode with 20 guard bits and rounds half up,
  `((a << 32) / b + 0x80000) >> 20` (`lib/NitroSDK/asm/fx_cp.s`
  `FX_DivAsync`/`FX_GetDivResult`) — not a truncating `(a << 12) / b`;
  for preset 4 the quotient 251.49 lands on 251 either way, but a
  preset whose tangent has a fraction ≥ .5 LSB rounds up. The
  aspect is `FX32_CONST(1.33333333)` = 5461/4096. Both presets scale
  ≈1 world unit to 1 pixel at the target depth, which is why the
  32-unit sprite quads are 32 px tall.
* **Viewport.** `G3_ViewPort(0, 0, 255, 191)`: NDC x → 0..256, NDC
  y → rows 192..0; depth is NDC z (smaller nearer), tested with
  `z ≤ stored + ε` so coplanar decals drawn later still show.
* **Viewport precision.** The production matrices are signed 20.12.
  Projected coordinates become integers before scan conversion. When
  W exceeds 16 bits the viewport divider drops one numerator bit, as
  the geometry engine does; depth is retained as a 24-bit integer.

### The billboard "shear" and map objects (`field/billboard.rs`)

`ov01_021E6220` computes `t = FX_Mul(8 << 12, FX_CosIdx(−angle.x))`
(`FieldSystem.unk11C = 8`) and adds `_22 · t` to the projection's
`_32` while the field effects and `BillboardLists_Draw` run
(`fieldmap.c:590-613`). In the row-vector convention `_32` is the
constant term of clip z, so this is a **depth bias of 8·cos(pitch)
world units toward the camera** (≈ 5.2 for the field pitches), not a
geometric shear: a quad anchored at a character's feet is otherwise
coplanar with the ground along its bottom row and loses the depth test
to it. `Camera::billboard_projection` carries exactly this matrix.

Map objects are the `mmodel` quads of `a/0/8/1` (832 NSBTX, 14 NSBMD).
`ov01_022074A8` (`asm/overlay_01_sprite_data.s:436`) maps a sprite to
its NSBTX and packs the size class into bits 10–15 of its third u16;
`sub_021FA248` resolves the class through `ov01_02207318`. The standard
class `mmdl_m32x32` (member 266) has one node and the SBC program
`NODEDESC, NODE, BB, POSSCALE, MAT, SHP, POSSCALE, RET` — opcode 7 is
`NNS_G3D_SBC_BB`, the full billboard: the node keeps its camera-space
translation and scale and its rotation becomes the identity *in camera
space*. The quad is `x ∈ [−16, 16], y ∈ [0, 32], z = 0` with texture
`v = 32` at the bottom (read from the ROM's display list), i.e.
anchored at the **feet**, 32 units tall, camera-facing. `BillboardView
{ texture, rect, world_pos, size_px }` places such a quad: the corners
are built in camera space from the projected anchor, projected through
the biased matrix, and rasterized as one GX quad. Which
texture, direction and walk frame to show is the map-object model's
concern; the view takes a texture and a texel rectangle.

The previous shim blitted the player image at a hand-tuned
`(−16, −28)` from the projected tile centre with a fixed depth bias of
8; it now stands on the quad's anchor with the original's bias, which
moves the sprite up by the 4 rows the old offset added.

### Rasterizer

GX triangles and quads retain their primitive identity through model
placement and enter an integer scanline converter directly; strips are
expanded to the same primitive kind, never to a synthetic quad
diagonal. Edge ownership depends on winding, slope and display state,
including one-row and overlapping-edge cases. Attributes use quantized
perspective factors, and texels use `TEXIMAGE_PARAM` repeat/flip rules.
The sampler retains the original texels/palette for all five formats
used by HeartGold. Native colour and alpha precision is six and five
bits respectively; indexed colour-zero transparency does not override
the explicit alpha in A3I5/A5I3 (GBATEK, TEXIMAGE_PARAM bit 29).

Model-local GX vertices and model/node/POSSCALE matrices remain separate
until camera submission. Baking vertices into world coordinates before
concatenating the matrices changes fixed-point rounding and is not
equivalent. Clipping anchors intersections at the outside endpoint so
shared edges produce identical intersections in either traversal direction.

Auto-sort uses opacity and polygon Y bounds, preserving submission order
on ties. Per-pixel alpha testing precedes writes. The two pixel layers
retain native depth, opaque/translucent polygon IDs, fog eligibility,
edge flags and coverage. Implemented paths include Z/W selection,
translucent depth-update flags, shadow stencil, toon/highlight, four-light
normal shading and shininess. Resolve applies edge marking, fog, then AA.
These paths have synthetic tests, **not yet complete hardware parity**.

The earlier AA-disabled claim was wrong for the live field: the captured
retail bedroom at VBlank 4822 has `DISP3DCNT=0x0039`, enabling texture,
alpha blending, antialiasing and edge marking. Its edge palette is black
for IDs 0–7 and `0x1084` for the other groups. Both the production
compositor and raw capture now obtain this configuration through
`field_registers`; the earlier initialization call to
`G3X_AntiAlias(FALSE)` is not the final live state.

Still outside the whole-core claim: complete GX command/matrix-stack
execution, per-command material changes and texture matrices, DIRECT and
COMP4x4 texture formats, and oracle-validated corner cases for lighting,
W depth, shadows, fog and AA. The retail ground-shadow effect is also
missing from field submission; having a shadow raster mode does not
create that geometry automatically.

### Seams for the map-data `FieldScene`

`SceneView { meshes: &[Mesh], billboards: Vec<BillboardView>, camera:
Camera }` is what `render_view` draws. Map cells are world-space meshes
parsed with their matrix-cell origin; props (`MapPropArcData { model,
translation, rotation, scale }`) are baked into world meshes with
pret's model matrix — `RotX(rotation.x) · RotY(rotation.y) ·
RotZ(rotation.z)` from the low 16 bits of each fx32 component as a
16-bit angle (`sub_02020D2C`, `asm/unk_02020B8C.s:218`), then scale and
translation (`GF3dRender_DrawModel`, `src/gf_3d_render.c:34`);
`camera::sin_idx`/`cos_idx` give the same table values. Billboards take
the decoded NSBTX (`model::decode_texture`), the frame rect, the
object's position vector and the size class' quad size.
`CameraPreset::from_table_entry` reads a 0x24-byte row of
`ov01_02206478`; `Camera::from_preset(preset, target)` resolves it.

### Verification

`tests/field_hg.rs` (ROM-gated) pins the bedroom through the whole
pipeline — NSBMD decode, camera, rasterizer, billboard, compositor at
BG0 priority 1 — by SHA-1 for both genders, checks the plane's
coverage and the sprite's depth, and renders the room through the
outdoor perspective preset; `APRICORN_RENDER_OUT` writes the review
PNGs (`field-bedroom-{0,1}.png`, `field-bedroom-outdoor.png`).
`tests/raster.rs` (no ROM) composites a synthetic one-quad field under
BG1 at lower and higher priority, under a sprite, with the plane
disabled and windowed, under master brightness and the blend fades,
and translucent over BG1 and the backdrop. The camera unit tests pin
the sine table (SHA-1), `FX_Mul`/`FX_Div`, the preset row layout, and
hand-derived projections for both presets.

## Presenters

### Headless: `apricorn-gfx-dump`

`apricorn-gfx-dump --rom <rom.nds> --frame N[,N..] --out <dir>` —
boots the chain, ticks inputless, rasterizes each requested frame,
writes top/bottom PNGs honoring the frame's `DisplaySelect`, and
prints each screen's SHA-1 over the raw RGBA. `scripts/demo.ps1`
dumps frames `0,45,100,135,200,320,420,1000` into `out/frames/` —
the testable manual check without a window.

### The desktop shell (`apricorn-desktop`, bin `apricorn`)

`cargo run -p apricorn-desktop [--rom <path>]`
— winit window +
wgpu present, the Phase 3 exit criterion. The presenter uploads both
screens to two 256×192 `Rgba8Unorm` textures and draws two quads of a
trivial WGSL blit; the layout is the largest integer scale of the
stacked 256×384 pair that fits, centered with black letterbox bars.
The surface format is deliberately the first *non-sRGB* one: the
rasterizer emits display-ready sRGB bytes, and an sRGB target would
re-encode them. `PresentMode::Fifo` (vsync) throttles redraws;
redraws are only requested when a tick produces a new frame.

Persistent desktop saves are a separate local workstream, not part of
this renderer PR. This branch leaves the desktop lifecycle unchanged.

The quads' UV mapping carries a pinned invariant: the quad's corner
(u,v)=(0,0) is its top-left in NDC *and* row 0 of the buffer — the
two top-lefts coincide, so no V flip belongs in the shader (a
spurious one is exactly how the screens-upside-down bug first
shipped, caught by the same window session as the palette swap).

## Input

The keyboard maps onto the `REG_KEYXY` bits the engine consumes
(physical-key based, so it survives OS layouts):

| Key | Button |
|---|---|
| A / B / X / Y | A / B / X / Y |
| Arrows | D-pad |
| Enter (main/numpad) | START |
| Shift (left/right) | SELECT |
| L / R | L / R |
| Escape | close window |

Touch is deferred with the touch model: the Phase 3 beats skip on
START/A anyway.

## Timestep

The DS refreshes at ~59.8268 Hz (GBATEK) — one tick every
16,714,917 ns, slightly *under* 60. `apricorn-desktop::runner::Pacer`
is the classic accumulator: elapsed wall time banks in a carry and
pays out one tick per period, clamped to 4 per poll (the
spiral-of-death guard — a stalled window catches up over a few
frames, then the debt is discarded: the chain catches up to *now*, not
to the paused interval, exactly as an emulator does). Backwards polls
(clock skew) owe nothing. The winit loop sleeps to the pacer's next
deadline (`ControlFlow::WaitUntil`) instead of spinning.

The engine never sees the clock: ticks are pure functions of the
frame index, so replaying a frame range reproduces the same logical
frames in the harness as the window showed live.

## Deferred (with where each lands)

* **3D engine / NSBMD** (title's Ho-Oh BG0, camera pan) → Phase 6;
  renders as the absent layer it is in the model.
* **Text renderer** ("TOUCH TO START" window content) → Phase 4.
* **OAM/sprite rendering** (modeled in `docs/nds-2d.md`, not
  rasterized) → with the first scene that needs sprites.
* **Intro movie scenes 2–5** (sprites, circle wipe, 3D) → their
  subsystem phases; Phase 3 ships the copyright beat only, per the
  scope decision.
* **Touch input** → with the touch model (first consumer is the
  title screen's "TOUCH" prompt, Phase 4+).
* **Audio** → the SDAT phases.
* **GPU compositing** — the CPU-rasterize/wgpu-present decision is
  deliberate (determinism first); a GPU renderer can replace the
  rasterizer behind the same `render` contract later, non-breaking.

## Equivalence story

Phase 3's parity instrument is the **golden raster hash**:
`tests/raster_hg.rs` (ROM-dependent, skip-silent) boots the chain and
SHA-1s both screens at the corpus frames — the copyright beat
(0/45/100/135/200) and the title statics (320/420/1000) — committing
**hashes only, never pixels**. The hashes pin the whole pipeline
end-to-end: loader, decoder, frame model, scene logic, rasterizer.
Because a `LogicalFrame` is pure data, the same discipline extends
upward: from Phase 4 the harness can hash logical-frame regions into
the trace corpus alongside the RAM-watch regions, giving
divergence-pointing comparison *below* pixels too.

The melonDS oracle can now dump the software 3D core before 2D
composition as `APGX3D1` files: native colour, depth, and polygon
attribute words for every 256×192 pixel. `Gx3dSnapshot` parses them and
can compare exact colour-change masks between consecutive frames. This
is the gate for renderer work; engine-only goldens pin determinism but
do not by themselves establish hardware equality.

The repeatable motion command and capture instructions are in
[`docs/oracle.md`](oracle.md#exact-motion-gate). The 2026-09-12 run is
**not exact**: ten bedroom frames have 29–63 colour, 0–5 depth and
28–39 attribute mismatches each. These are measurements of the original
combined local worktree, not proof of this isolated PR's parity. Coverage matches in all ten; eight
transition masks match and one differs at one pixel. Existing field
goldens remain unmodified rather than blessing those mismatches.

The renderer-only performance gate is:

```
cargo test -p apricorn-gfx --release --test field_system_hg new_bark_release_frame_budget -- --ignored --nocapture
```

It warms up 24 frames, then times 120 New Bark frames without disk I/O,
loading or simulation. Historical combined-worktree result: mean 4.113 ms, p95 5.191 ms,
maximum 6.072 ms. The assertion requires every measured frame below
16.714 ms. Composition/presentation and desktop backlog need separate
measurement; this result must not be presented as their verification.

## Versions

Pinned in `Cargo.lock` (the plan's wgpu/winit drift risk): wgpu
30.0.1, winit 0.30.13, pollster 1.0.1. The wgpu surface API is small
and isolated in `presenter.rs`; drift lands as compile errors there,
not as silent behavior changes.

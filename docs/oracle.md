# The oracle — a headless melonDS 1.1 that proves the whole machine

The oracle is the reference NDS in apricorn's differential-testing harness:
a **patched, core-only build of melonDS 1.1** that boots the real ROM
headlessly, replays scripted input, hashes watched memory regions, and emits
a trace the comparator (`apricorn-diff`) checks against the engine side.
Where [`arm-runner`](arm-runner.md) proves individual functions, the oracle
proves the *whole machine* — timing, interrupts, both CPUs, and the boot
sequence included.

It is never shipped, never committed as a binary, and never built with the
ROM's data inside it: the clone lives in `refs/melonds` (gitignored), build
artifacts in `out/oracle` (gitignored), and the ROM is read at run time like
everywhere else in the project.

## Why melonDS, and why these build flags

melonDS 1.1's `src/CMakeLists.txt` defines a static `core` library covering
the entire machine — and its Qt/SDL frontend is behind the `BUILD_QT_SDL`
option. The oracle is therefore not a stripped frontend but a **new `main`
plus a platform layer**, both added by the committed patch:

- `src/apricorn-oracle.cpp` — the `main`: boot, input injection, frame loop,
  region hashing, probes, trace emission.
- `src/apricorn-platform.cpp` — a headless implementation of melonDS's
  `Platform` interface. The core *calls* `Platform::` for everything
  host-side, but every real implementation lives in the Qt/SDL frontend —
  so a core-only build needs its own. Files get real stdio, logging goes to
  stderr, threads/mutexes/semaphores are the real C++11 primitives, and
  everything that would reach the outside world (mic, camera, wifi/network
  packets, save/firmware writes) is a deterministic no-op: the oracle is
  read-only and touches nothing on the host beyond its own output file.

The determinism contract is the configure line in
`oracle/setup.ps1` / `oracle/setup.sh`:

```
cmake -S refs/melonds -B out/oracle -G Ninja \
    -DCMAKE_BUILD_TYPE=Release \
    -DBUILD_QT_SDL=OFF     \  no window, no SDL, no Qt
    -DENABLE_JIT=OFF        \  pure interpreter — no JIT nondeterminism
    -DENABLE_OGLRENDERER=OFF\  software 3D renderer
    -DENABLE_GDBSTUB=OFF    \  no debugger hooks
    -DMELONDS_EMBED_BUILD_INFO=OFF
```

With those flags the core has no host-time and no `rand()` dependency on any
path an NDS game touches (the frontend's `srand(time(nullptr))`, the GBA
cart FAT timestamps, and the wifi network stack are all outside this build).
On Windows the binary statically links the C++ runtime, so once built it
runs without the MSYS2 toolchain on `PATH` (the *build* still needs
`mingw64/bin` there — `cc1.exe` can't find its DLLs otherwise).

## Boot: DirectBoot, generated firmware, pinned RTC

The oracle boots exactly like the reference frontend's direct-boot path:

1. `NDSCart::ParseROM` → `NDS::SetNDSCart`
2. `NDS::Reset()`
3. `RTC::SetDateTime(...)` — the **pinned RTC** (`--rtc`, default
   `2010-03-01T09:00:00`). The core's RTC never reads host time: it starts
   from a fixed default and ticks with emulated cycles, so pinning the start
   pins the whole clock.
4. `NDS::SetupDirectBoot(romname)` — no BIOS or firmware dump involved:
   `NDSArgs` defaults to FreeBIOS and melonDS's **generated firmware**
   (fixed username/birthday/MAC), both deterministic. Generated firmware
   is not BIOS-bootable, which is fine — direct boot is the point.
5. `NDS::Start()`

## The trace

The output matches `crates/apricorn-harness/src/trace.rs` byte for byte
(that module's round-trip test is the reference grammar). The oracle
computes `rom-sha1` itself; `input-sha1` and `regions-sha1` are
**passthrough values** — the hashes of the canonical `input.apin` /
`regions.conf` text files, supplied by the Rust side, because they gate
comparisons between producers and only the Rust harness knows the text
formats' canonical forms.

If the emulated console stops mid-run (GBA mode, bad exception, power-off),
the oracle ends the trace at that frame and exits 3 — it never invents
frames a stopped machine didn't produce.

## Input blobs (little-endian, all)

The Rust harness owns every text format; the oracle consumes compiled
blobs so C++ never re-implements parsing. All multi-byte values are
little-endian.

**Input blob** (`--input`, optional — absent means no input):

```
u32  frame_count
repeat frame_count times:
    u16  keymask       // bits 0..11, bit set = HELD:
                      // A B Select Start Right Left Up Down R L X Y
    u8   touch_down   // 0 = released
    u8   padding
    u16  touch_x
    u16  touch_y
```

Frames beyond `frame_count` get all-zero input (boot-idle).

The keymask is the harness's canonical "bit set = held" form
(`input.rs`). melonDS's `NDS::SetKeyMask` stores its argument straight
into the **active-low** `KEYINPUT`/`EXTKEYIN` registers (bit set =
released; the Qt frontend starts from `0xFFF` and clears bits on press),
so the oracle inverts at the edge: `SetKeyMask(~keymask & 0xFFF)`. The
first oracle passed the mask through unchanged — every key held from
power-on — and HeartGold's L+R+START+SELECT soft reset rebooted the
game every ~220 frames (the boot RNG states returning to their zeroed
hashes in the old `boot-idle` trace were that loop, not the title
screen). The screenshot facility below is what exposed it.

**Regions blob** (`--regions`):

```
u32  count
repeat count times:
    u32  addr
    u32  size
    u32  sample       // hash every `sample` frames; 0 = every frame
    u32  name_len
    u8[name_len]  name  // the F record's region name
```

**Probes blob** (`--probes`, optional):

```
u32  count
repeat count times:
    u32  frame       // run at this frame's boundary, before its input
    u32  entry       // ARM9 entry address, bit 0 = Thumb
    u32  nargs       // 0..4
    u32  args[4]
    u32  name_len
    u8[name_len]  name  // the C record's fn name (a pins table entry)
```

## Probe mechanics (`C` records)

A probe hijacks the ARM9 at a frame boundary exactly the way arm-runner
enters a leaf in its own interpreter: r0–r3 from the probe, r12 = 0,
r13 = 0x02300000, r14 = sentinel, `JumpTo(entry)` — then single-steps
(`ARM9Target = ARM9Timestamp + 1`) until the instruction address equals the
sentinel or a 4M-step budget dies, and restores registers, CPSR, cycle
counters, timestamps, and IME afterwards. The sentinel is `0x02300000` —
mapped, unlike arm-runner's `0xFFFF0000`, because melonDS prefetches code
eagerly at `JumpTo`; pushes from the probe stack land below it and the loop
stops before the sentinel is ever fetched. The differing sentinel never
appears in a trace — only r0–r3 and a state hash do.

The `state` field of a `C` record is SHA-1 over the **concatenation of all
regions** in `regions.conf` order, read via the ARM9 bus after the call.
arm-runner computes the identical digest over its own memory in the Rust
probe (Phase 2 step 7).

IRQs are masked (`IME[0] = 0`, CPSR `I` bit) for the probe's duration; a
pending IRQ must not steal the PC mid-probe.

**Probe timing — never at frame 0.** DirectBoot enters the game at its
LZ-compressed ARM9 static main; crt0 decompresses it in place over the
first few frames, so the pinned addresses hold compressed garbage until
that finishes (a frame-0 probe dies in the step budget on an
"undefined instruction" in what should be real code). Twenty frames in,
the pinned functions are real code on both machines — the committed
probe schedules and the differential tests use frame ≥ 20. There is no
detector for "decompression finished"; the safe margin is a pinned
constant of the corpus, verified by the pins' prologue hashes whenever
arm-runner loads the image.

## Determinism guarantees

Equivalence is frame-indexed, never wall-clock: the oracle runs N
`NDS::RunFrame()` calls and counts VBlanks. Two runs with the same ROM,
input, regions, RTC, and probe list produce byte-identical traces —
asserted by the harness's determinism test — and a different pinned RTC
must produce a different trace (the RTC-pin test), since HeartGold's boot
seeds from the clock.

## Screenshots (`--shots`)

```
apricorn-oracle run … --shots 120,300,305 --shots-dir out/oracle-shots/x
```

At the **end** of each listed frame — after that frame's `RunFrame`,
after the stop check, before the region hashes, i.e. exactly the
machine state the frame's `F` records hash — the oracle writes both
LCDs as `<dir>/frame_%06u_top.png` and `<dir>/frame_%06u_bottom.png`:
256×192, 8-bit RGB, no alpha. Frames at or past `--frames` are never
written (a warning on stderr). The trace is **byte-identical** with or
without `--shots` (asserted by `oracle_hg.rs`); the PNGs are review
artifacts — ground truth for comparing engine renders — and live under
`out/` (gitignored), never in the corpus.

Where the pixels come from (verified in `GPU2D_Soft.cpp`'s final pass,
melonDS 1.1): the software renderer leaves a completed frame in
`GPU.Framebuffer[GPU.FrontBuffer][0 = top, 1 = bottom]` as `0xFFRRGGBB`
words — the DS's 6-bit channels expanded to 8 with the top two bits
replicated into the bottom two, red in bits 16–23, green 8–15, blue 0–7,
alpha `0xFF` ("BGRA" only as a little-endian byte order).
`FrontBuffer` flips at every `FinishFrame`, so after `RunFrame` it
indexes the frame just finished — the same buffer the Qt frontend
displays. `Framebuffer[..][0]` is always the *top* LCD:
`AssignFramebuffers` routes the two engines by `POWCNT1`'s display
swap, so the file names follow the physical screens, not engines A/B.

The core-only build does not link zlib, so the encoder in
`apricorn-oracle.cpp` is self-contained: signature, `IHDR`, one `IDAT`
holding a zlib stream of **stored** (uncompressed) deflate blocks —
header `78 01`, ≤65535-byte blocks with `LEN`/`NLEN`, Adler-32 trailer —
and `IEND`, each chunk CRC-32'd. Every row carries filter type 0.
147,726 bytes per LCD.

Rust side: [`oracle::ShotRequest`] (`frames` + `dir`) passed to
`OracleRun::run_with_shots` / `Case::run_oracle_with_shots`; the
directory is created first. `apricorn-replay --shots FRAMES --shots-dir
DIR <case>` exposes it on the command line — `FRAMES` is a comma list
of frame indices or inclusive `first-last` ranges (`120,300-305`),
expanded before the oracle sees it. `scripts/shots.ps1` wraps that.
`APRICORN_ORACLE=<path>` overrides the `out/oracle` lookup, so a
scratch build (`cmake -B out/oracle-dev …`) can be driven through the
harness before it replaces the shared binary.

The worked example is `corpus/new-game` — power-on to the bedroom on
the retail ROM, authored entirely from screenshots: coarse `--shots`
scans (every 10–20 frames) located each screen, per-frame scans pinned
every transition, and the input was extended one step at a time until
the bedroom's first fully-faded-in frame. Its `README.md` records every
milestone frame with the exact input events, and the review set it
names is the visual ground truth for the engine's new-game path.

## Raw 3D snapshots (`--gx-shots`)

```
apricorn-replay --gx-shots 4816-4824 --gx-shots-dir out/oracle-gx corpus/new-game
```

The raw snapshot path bypasses engine A/B composition. It reads the
completed software 3D renderer's topmost colour, depth, and attribute
buffers and writes `<dir>/frame_%06u_gx3d.bin`. The format is
little-endian:

```
u8[8] magic = "APGX3D1\0"
u32   width = 256
u32   height = 192
repeat width * height times, row-major:
    u32 color
    u32 depth
    u32 attributes
```

`color` is melonDS's native packed software-renderer word. `depth` is
the value used by its depth test. `attributes` retains edge coverage,
fog, translucency, and polygon-id bits. The accessors exist only in the
patched oracle build; no melonDS code or data is linked into apricorn.

Rust side: `oracle::GxShotRequest` drives capture and
`oracle::Gx3dSnapshot` validates and parses it. `color_change_mask`
produces one boolean per pixel, allowing a sequence to require that
both every frame and every inter-frame changed-pixel mask equals the
oracle. LCD screenshots and raw snapshots may be requested together.

Each raw capture also writes `frame_%06u_gxgeo.bin` (magic `APGXGEO1`):
five u32 words for display control, clear colour/attributes, clear depth,
flush attributes and polygon count; three 16-word matrices (projection,
current position, current clip); then the sorted polygon records. Each
record has eight u32 words (polygon attributes, texture parameters,
palette base, vertex count, type, sort key, W-buffer flag, diagnostic
flags), followed by thirteen words per vertex (clip XYZW, screen XY,
depth, W, colour RGB, texture UV). Signed words use two's complement.
The current matrices are the last submitted GX state, **not necessarily
the camera matrices**. `gxregs.bin` is an unversioned diagnostic dump
of eight edge colours, 32 toon colours, fog colour, fog offset and shift.
These artifacts expose observed oracle state, not renderer code.

### Exact motion gate

```
cargo run -p apricorn-harness --bin apricorn-gx-diff -- --sequence oracle/sequences/bedroom-down.tsv
```

The TSV contains expected/actual paths relative to its own directory, in
display order. Repeated frames must remain in the list. Every frame is
compared across colour/alpha, depth and attributes, and every transition
compares the exact colour-change mask. Exit 0 means all comparisons are
exact, 1 means a mismatch, and 64 means invalid/missing input. Empty and
single-frame manifests cannot pass a motion gate. Do not crop furniture,
mask shadows, apply tolerance, or update engine goldens to bypass a failure.

Generate the current bedroom pair set with:

```powershell
cargo run -p apricorn-harness --bin apricorn-replay -- --rom hg_usa.nds --hold DOWN 4808-4850 --gx-shots 4808-4850 --gx-shots-dir out/oracle-gx-motion-down corpus/new-game
$env:APRICORN_RENDER_OUT = Join-Path (Get-Location) 'out/ds-render-review'
cargo test -p apricorn-gfx --test field_system_hg bedroom_walk_has_a_pinned_contiguous_raster_sequence -- --nocapture
```

The engine test writes all artifacts before checking its older goldens.
Those goldens are intentionally not approved replacements yet: this
sequence currently fails exact parity. `--hold` changes only the in-memory
capture input, not the corpus baseline; its last frame is the release event.

For visual review also run `house_furniture_contiguous_review_sequence`
and `new_bark_arrival_golden` with the same output directory. Inspect the
actual PNGs yourself, including multiple camera offsets outdoors. This
is complementary to the exact gate, never a substitute.

## Usage

```
apricorn-oracle run --rom hg_usa.nds --out trace.txt
    --regions regions.bin [--input input.bin] [--probes probes.bin]
    [--frames 600] [--rtc 2010-03-01T09:00:00]
    --input-sha1 HEX --regions-sha1 HEX [--producer oracle-melonds-1.1]
    [--shots F,F,... --shots-dir DIR]
    [--gx-shots F,F,... --gx-shots-dir DIR]
```

Exit 0 on success, 2 on runtime failure (bad blob, unreadable ROM,
unwritable screenshot), 3 when the machine stopped before the requested
frame count, 64 on usage errors (including a screenshot flag without
its directory or a malformed frame list).

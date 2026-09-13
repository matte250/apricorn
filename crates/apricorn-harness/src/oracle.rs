//! Glue to the oracle binary: compile the harness's text formats into
//! the little-endian blobs the patched melonDS consumes, locate the
//! build, shell out, and parse the trace it writes back.
//!
//! The Rust side owns every text format ([`crate::input`],
//! [`crate::regions`]); the oracle never parses text — this module is
//! the single compiler for the blob contracts documented in
//! `docs/oracle.md`:
//!
//! - regions: `u32 count`, then per region `u32 addr, size, sample,
//!   name_len` + name bytes;
//! - input: `u32 frame_count`, then per frame `u16 keymask, u8
//!   touch_down, u8 pad, u16 x, u16 y` (frames beyond the list get
//!   all-zero input — boot-idle);
//! - probes: `u32 count`, then per probe `u32 frame, entry, nargs,
//!   args[4], name_len` + name bytes.
//!
//! The trace header's `input-sha1` / `regions-sha1` are passthrough
//! hashes of the canonical text files, computed here — they gate diff
//! pairs, and only this side knows the canonical forms.
//!
//! A run can also ask for composited LCD screenshots ([`ShotRequest`])
//! or raw software-3D planes ([`GxShotRequest`]). Both are frame-indexed
//! review/test artifacts and never change the trace.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};

use crate::HarnessError;
use crate::input::{FrameInput, InputScript};
use crate::regions::RegionSet;
use crate::trace::Trace;

/// One scheduled function probe (the source of a `C` record): call
/// `name` at `entry` (bit 0 = Thumb) with `args` (0..=4) at `frame`'s
/// boundary, before that frame's input and execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeSpec {
    /// Frame boundary to run the probe at.
    pub frame: u32,
    /// Entry address; bit 0 selects Thumb.
    pub entry: u32,
    /// Arguments r0..r3 (fewer than 4 leave the rest zeroed).
    pub args: Vec<u32>,
    /// The function's name — a `pins` table entry; the trace's `fn`.
    pub name: String,
}

/// A screenshot request: both LCDs as 256×192 RGB8 PNGs at the end of
/// each listed frame — after that frame's `RunFrame`, exactly the
/// machine state the trace's `F` records hash. Files land in `dir` as
/// `frame_NNNNNN_top.png` / `frame_NNNNNN_bottom.png` (six-digit,
/// zero-padded frame index). The trace is byte-identical with or
/// without a request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShotRequest {
    /// Frames to capture. Frames at or past the run length are never
    /// written (the oracle warns on stderr); duplicates are harmless.
    pub frames: Vec<u32>,
    /// Output directory; created (with parents) before the run.
    pub dir: PathBuf,
}

impl ShotRequest {
    /// The top-LCD PNG path for `frame` under `dir`.
    #[must_use]
    pub fn top_path(&self, frame: u32) -> PathBuf {
        self.dir.join(format!("frame_{frame:06}_top.png"))
    }

    /// The bottom-LCD PNG path for `frame` under `dir`.
    #[must_use]
    pub fn bottom_path(&self, frame: u32) -> PathBuf {
        self.dir.join(format!("frame_{frame:06}_bottom.png"))
    }

    /// The oracle's `--shots` value: the frames sorted, deduplicated,
    /// comma-separated.
    #[must_use]
    pub fn frames_arg(&self) -> String {
        frames_arg(&self.frames)
    }
}

/// A raw software-3D snapshot request. Each file contains the native
/// colour, depth and polygon-attribute planes before 2D composition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GxShotRequest {
    /// Frames to capture. Duplicates are harmless.
    pub frames: Vec<u32>,
    /// Output directory; created (with parents) before the run.
    pub dir: PathBuf,
}

impl GxShotRequest {
    /// The raw snapshot path for `frame` under `dir`.
    #[must_use]
    pub fn path(&self, frame: u32) -> PathBuf {
        self.dir.join(format!("frame_{frame:06}_gx3d.bin"))
    }

    /// The oracle's sorted, deduplicated `--gx-shots` value.
    #[must_use]
    pub fn frames_arg(&self) -> String {
        frames_arg(&self.frames)
    }
}

/// One native 256×192 software-renderer snapshot from the oracle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Gx3dSnapshot {
    width: u32,
    height: u32,
    color: Box<[u32]>,
    depth: Box<[u32]>,
    attributes: Box<[u32]>,
}

/// Exact per-plane difference counts between two raw GX snapshots.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Gx3dDiff {
    /// Pixels whose native R6/G6/B6/A5 word differs.
    pub color_pixels: usize,
    /// Pixels whose depth word differs.
    pub depth_pixels: usize,
    /// Pixels whose edge/fog/polygon metadata differs.
    pub attribute_pixels: usize,
    /// Pixels where one image is clear and the other is covered.
    pub coverage_pixels: usize,
}

impl Gx3dDiff {
    /// Whether all three native planes are byte-for-byte equivalent.
    #[must_use]
    pub const fn is_exact(self) -> bool {
        self.color_pixels == 0 && self.depth_pixels == 0 && self.attribute_pixels == 0
    }
}

impl Gx3dSnapshot {
    /// Parses an `APGX3D1` little-endian snapshot.
    ///
    /// # Errors
    /// Returns a gate error when the magic, dimensions, or payload size
    /// is invalid.
    pub fn parse(bytes: &[u8]) -> Result<Self, HarnessError> {
        const HEADER: usize = 16;
        if bytes.get(..8) != Some(b"APGX3D1\0") {
            return Err(HarnessError::Gate {
                what: "raw GX snapshot has invalid magic".to_string(),
            });
        }
        if bytes.len() < HEADER {
            return Err(HarnessError::Gate {
                what: "raw GX snapshot is truncated".to_string(),
            });
        }
        let read = |offset: usize| {
            u32::from_le_bytes(bytes[offset..offset + 4].try_into().expect("four bytes"))
        };
        let width = read(8);
        let height = read(12);
        if width == 0 || height == 0 {
            return Err(HarnessError::Gate {
                what: "raw GX snapshot dimensions must be nonzero".to_string(),
            });
        }
        let pixels = usize::try_from(u64::from(width) * u64::from(height)).map_err(|_| {
            HarnessError::Gate {
                what: "raw GX snapshot dimensions overflow".to_string(),
            }
        })?;
        let expected = HEADER
            .checked_add(pixels.checked_mul(12).ok_or_else(|| HarnessError::Gate {
                what: "raw GX snapshot payload overflows".to_string(),
            })?)
            .ok_or_else(|| HarnessError::Gate {
                what: "raw GX snapshot size overflows".to_string(),
            })?;
        if bytes.len() != expected {
            return Err(HarnessError::Gate {
                what: format!(
                    "raw GX snapshot is {} bytes; expected {expected} for {width}x{height}",
                    bytes.len()
                ),
            });
        }

        let mut color = Vec::with_capacity(pixels);
        let mut depth = Vec::with_capacity(pixels);
        let mut attributes = Vec::with_capacity(pixels);
        for pixel in bytes[HEADER..].chunks_exact(12) {
            color.push(u32::from_le_bytes(
                pixel[0..4].try_into().expect("four bytes"),
            ));
            depth.push(u32::from_le_bytes(
                pixel[4..8].try_into().expect("four bytes"),
            ));
            attributes.push(u32::from_le_bytes(
                pixel[8..12].try_into().expect("four bytes"),
            ));
        }
        Ok(Self {
            width,
            height,
            color: color.into_boxed_slice(),
            depth: depth.into_boxed_slice(),
            attributes: attributes.into_boxed_slice(),
        })
    }

    /// Loads and parses a snapshot file.
    ///
    /// # Errors
    /// Returns a gate error when the file cannot be read or parsed.
    pub fn read(path: &Path) -> Result<Self, HarnessError> {
        let bytes = std::fs::read(path).map_err(|error| HarnessError::Gate {
            what: format!("cannot read {}: {error}", path.display()),
        })?;
        Self::parse(&bytes)
    }

    /// Snapshot width in pixels.
    #[must_use]
    pub const fn width(&self) -> u32 {
        self.width
    }

    /// Snapshot height in pixels.
    #[must_use]
    pub const fn height(&self) -> u32 {
        self.height
    }

    /// Native packed software-renderer colour words.
    #[must_use]
    pub fn colors(&self) -> &[u32] {
        &self.color
    }

    /// Native depth words used by depth tests.
    #[must_use]
    pub fn depths(&self) -> &[u32] {
        &self.depth
    }

    /// Native attribute words containing edge, fog, and polygon metadata.
    #[must_use]
    pub fn attributes(&self) -> &[u32] {
        &self.attributes
    }

    /// Returns the exact colour-change mask between two frames.
    ///
    /// # Errors
    /// Returns a gate error when the dimensions differ.
    pub fn color_change_mask(&self, next: &Self) -> Result<Vec<bool>, HarnessError> {
        if (self.width, self.height) != (next.width, next.height) {
            return Err(HarnessError::Gate {
                what: format!(
                    "GX snapshot dimensions differ: {}x{} versus {}x{}",
                    self.width, self.height, next.width, next.height
                ),
            });
        }
        Ok(self
            .color
            .iter()
            .zip(next.color.iter())
            .map(|(before, after)| before != after)
            .collect())
    }

    /// Counts exact native-plane differences against `other`.
    ///
    /// # Errors
    /// Returns a gate error when the dimensions differ.
    pub fn diff(&self, other: &Self) -> Result<Gx3dDiff, HarnessError> {
        if (self.width, self.height) != (other.width, other.height) {
            return Err(HarnessError::Gate {
                what: format!(
                    "GX snapshot dimensions differ: {}x{} versus {}x{}",
                    self.width, self.height, other.width, other.height
                ),
            });
        }
        let mut diff = Gx3dDiff::default();
        for index in 0..self.color.len() {
            let color = self.color[index];
            let depth = self.depth[index];
            let attribute = self.attributes[index];
            let other_color = other.color[index];
            let other_depth = other.depth[index];
            let other_attribute = other.attributes[index];
            diff.color_pixels += usize::from(color != other_color);
            diff.depth_pixels += usize::from(depth != other_depth);
            diff.attribute_pixels += usize::from(attribute != other_attribute);
            let covered = color >> 24 != 0;
            let other_covered = other_color >> 24 != 0;
            diff.coverage_pixels += usize::from(covered != other_covered);
        }
        Ok(diff)
    }
}

fn frames_arg(frames: &[u32]) -> String {
    let mut frames = frames.to_vec();
    frames.sort_unstable();
    frames.dedup();
    frames
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(",")
}

/// Parses a frame list as the `apricorn-replay --shots` flag takes it:
/// comma-separated items, each a frame index or an inclusive
/// `first-last` range (`120,300-305` → 120, 300, 301, …, 305).
///
/// # Errors
/// Returns a [`HarnessError::Syntax`] (line 1) for an empty item, a
/// non-numeric bound, or a range whose `last` precedes `first`.
pub fn parse_shot_frames(text: &str) -> Result<Vec<u32>, HarnessError> {
    let bad = |what: String| HarnessError::Syntax { line: 1, what };
    let mut frames = Vec::new();
    for item in text.split(',') {
        let item = item.trim();
        let bound = |s: &str| -> Result<u32, HarnessError> {
            s.parse()
                .map_err(|_| bad(format!("--shots: '{s}' is not a frame index")))
        };
        match item.split_once('-') {
            Some((first, last)) => {
                let (first, last) = (bound(first.trim())?, bound(last.trim())?);
                if last < first {
                    return Err(bad(format!("--shots: range {first}-{last} runs backwards")));
                }
                frames.extend(first..=last);
            }
            None => frames.push(bound(item)?),
        }
    }
    Ok(frames)
}

/// Locates the built oracle under `out/oracle/` (gitignored; built by
/// `oracle/setup.ps1`). `None` when absent — callers skip silently.
///
/// `APRICORN_ORACLE`, when set to an existing file, overrides the
/// lookup — for driving a scratch build (`out/oracle-dev`) through the
/// harness before it replaces the shared one.
#[must_use]
pub fn find_binary() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("APRICORN_ORACLE") {
        let path = PathBuf::from(path);
        if path.is_file() {
            return Some(path);
        }
    }
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    for name in ["apricorn-oracle.exe", "apricorn-oracle"] {
        let path = repo.join("out/oracle").join(name);
        if path.is_file() {
            return Some(path);
        }
    }
    None
}

fn push_u32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}

/// Compiles the regions blob. Every region in file order.
#[must_use]
pub fn regions_blob(set: &RegionSet) -> Vec<u8> {
    let regions = set.regions();
    let mut blob = Vec::new();
    push_u32(&mut blob, regions.len() as u32);
    for r in regions {
        push_u32(&mut blob, r.address);
        push_u32(&mut blob, r.size);
        push_u32(&mut blob, r.sample);
        push_u32(&mut blob, r.name.len() as u32);
        blob.extend_from_slice(r.name.as_bytes());
    }
    blob
}

/// Compiles the input blob for `frames` frames: the script's state at
/// each frame, then all-zero input for frames past its `end`.
#[must_use]
pub fn input_blob(script: &InputScript, frames: u32) -> Vec<u8> {
    let mut blob = Vec::new();
    push_u32(&mut blob, frames);
    for frame in 0..frames {
        let FrameInput { mask, touch } = if frame < script.end {
            script.at(frame)
        } else {
            FrameInput::IDLE
        };
        blob.extend_from_slice(&mask.to_le_bytes());
        let (down, x, y) = match touch {
            Some((x, y)) => (1u8, x, y),
            None => (0u8, 0, 0),
        };
        blob.push(down);
        blob.push(0); // padding — part of the format contract
        blob.extend_from_slice(&x.to_le_bytes());
        blob.extend_from_slice(&y.to_le_bytes());
    }
    blob
}

/// Compiles the probes blob. `args` pads to four; more than four is a
/// programming error caught here rather than on the C++ side.
///
/// # Errors
/// Returns a [`HarnessError::Gate`] naming the probe if it carries
/// more than 4 arguments.
pub fn probes_blob(probes: &[ProbeSpec]) -> Result<Vec<u8>, HarnessError> {
    let mut blob = Vec::new();
    push_u32(&mut blob, probes.len() as u32);
    for p in probes {
        if p.args.len() > 4 {
            return Err(HarnessError::Gate {
                what: format!("probe {}: more than 4 args", p.name),
            });
        }
        push_u32(&mut blob, p.frame);
        push_u32(&mut blob, p.entry);
        push_u32(&mut blob, p.args.len() as u32);
        for a in 0..4 {
            push_u32(&mut blob, p.args.get(a).copied().unwrap_or(0));
        }
        push_u32(&mut blob, p.name.len() as u32);
        blob.extend_from_slice(p.name.as_bytes());
    }
    Ok(blob)
}

/// One oracle invocation.
#[derive(Debug, Clone)]
pub struct OracleRun<'a> {
    /// The ROM dump to boot.
    pub rom: &'a Path,
    /// The watched regions (hash source + blob + passthrough hash).
    pub regions: &'a RegionSet,
    /// The input script; `None` boots with no input at all.
    pub input: Option<&'a InputScript>,
    /// The probes to run, in order.
    pub probes: &'a [ProbeSpec],
    /// How many frames to run.
    pub frames: u32,
    /// The pinned RTC (`YYYY-MM-DDTHH:MM:SS`); `None` uses the oracle's
    /// documented default.
    pub rtc: Option<&'a str>,
    /// The `producer` name for the trace header; `None` uses the
    /// oracle's own default.
    pub producer: Option<&'a str>,
}

/// Distinct scratch filenames per process, and per concurrent run.
static RUN_COUNTER: AtomicU32 = AtomicU32::new(0);

impl OracleRun<'_> {
    /// The `input-sha1` passthrough this run will pin: the script's
    /// canonical-text hash, or the hash of the canonical empty script
    /// when there is no input.
    #[must_use]
    pub fn input_sha1_hex(&self) -> String {
        match self.input {
            Some(script) => script.sha1_hex(),
            None => InputScript {
                rtc: None,
                end: 0,
                events: Vec::new(),
            }
            .sha1_hex(),
        }
    }

    /// The `regions-sha1` passthrough: the canonical `regions.conf`
    /// hash of this run's region set.
    #[must_use]
    pub fn regions_sha1_hex(&self) -> String {
        self.regions.sha1_hex()
    }

    /// Runs the oracle and parses the trace it writes.
    ///
    /// Scratch blobs and the raw trace go to the system temp dir and
    /// are removed afterwards. `--frames 0` is valid and produces a
    /// probes-only trace (no `F` records).
    ///
    /// # Errors
    /// Returns a [`HarnessError::Gate`] when the oracle is missing,
    /// exits nonzero (its stderr is included), or writes a malformed
    /// trace ([`HarnessError::Syntax`]).
    pub fn run(&self) -> Result<Trace, HarnessError> {
        self.run_impl(None, None)
    }

    /// Like [`run`](Self::run), also writing the screenshots `shots`
    /// asks for (their directory is created first). The trace is the
    /// same one `run` would produce.
    ///
    /// # Errors
    /// As [`run`](Self::run), plus a [`HarnessError::Gate`] when the
    /// shots directory cannot be created.
    pub fn run_with_shots(&self, shots: &ShotRequest) -> Result<Trace, HarnessError> {
        self.run_impl(Some(shots), None)
    }

    /// Like [`run`](Self::run), also writing raw software-3D snapshots.
    ///
    /// # Errors
    /// As [`run`](Self::run), plus a [`HarnessError::Gate`] when the
    /// snapshot directory cannot be created.
    pub fn run_with_gx_shots(&self, shots: &GxShotRequest) -> Result<Trace, HarnessError> {
        self.run_impl(None, Some(shots))
    }

    /// Runs while writing both composited LCD screenshots and raw 3D
    /// snapshots.
    ///
    /// # Errors
    /// As [`run`](Self::run), plus a [`HarnessError::Gate`] when an
    /// artifact directory cannot be created.
    pub fn run_with_artifacts(
        &self,
        shots: &ShotRequest,
        gx_shots: &GxShotRequest,
    ) -> Result<Trace, HarnessError> {
        self.run_impl(Some(shots), Some(gx_shots))
    }

    fn run_impl(
        &self,
        shots: Option<&ShotRequest>,
        gx_shots: Option<&GxShotRequest>,
    ) -> Result<Trace, HarnessError> {
        let Some(binary) = find_binary() else {
            return Err(HarnessError::Gate {
                what: "oracle binary not built (run oracle/setup.ps1)".to_string(),
            });
        };
        let id = RUN_COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("apricorn-oracle-{}-{id}", std::process::id()));
        std::fs::create_dir_all(&dir).map_err(|e| HarnessError::Gate {
            what: format!("cannot create {}: {e}", dir.display()),
        })?;
        // Best-effort cleanup on every path.
        let cleanup = |dir: &Path| {
            let _ = std::fs::remove_dir_all(dir);
        };

        let regions_path = dir.join("regions.bin");
        let out_path = dir.join("out.trace");
        if let Err(e) = std::fs::write(&regions_path, regions_blob(self.regions)) {
            cleanup(&dir);
            return Err(HarnessError::Gate {
                what: format!("cannot write {}: {e}", regions_path.display()),
            });
        }

        let input_path = dir.join("input.bin");
        let has_input = self.input.is_some();
        if has_input
            && let Err(e) = std::fs::write(
                &input_path,
                input_blob(self.input.expect("checked above"), self.frames),
            )
        {
            cleanup(&dir);
            return Err(HarnessError::Gate {
                what: format!("cannot write {}: {e}", input_path.display()),
            });
        }

        let probes_path = dir.join("probes.bin");
        let has_probes = !self.probes.is_empty();
        let probes = match probes_blob(self.probes) {
            Ok(p) => p,
            Err(e) => {
                cleanup(&dir);
                return Err(e);
            }
        };
        if has_probes && let Err(e) = std::fs::write(&probes_path, probes) {
            cleanup(&dir);
            return Err(HarnessError::Gate {
                what: format!("cannot write {}: {e}", probes_path.display()),
            });
        }

        let mut cmd = Command::new(&binary);
        cmd.arg("run")
            .arg("--rom")
            .arg(self.rom)
            .arg("--out")
            .arg(&out_path)
            .arg("--regions")
            .arg(&regions_path)
            .arg("--frames")
            .arg(self.frames.to_string())
            .arg("--input-sha1")
            .arg(self.input_sha1_hex())
            .arg("--regions-sha1")
            .arg(self.regions_sha1_hex());
        if has_input {
            cmd.arg("--input").arg(&input_path);
        }
        if has_probes {
            cmd.arg("--probes").arg(&probes_path);
        }
        if let Some(rtc) = self.rtc {
            cmd.arg("--rtc").arg(rtc);
        }
        if let Some(producer) = self.producer {
            cmd.arg("--producer").arg(producer);
        }
        if let Some(shots) = shots.filter(|s| !s.frames.is_empty()) {
            if let Err(e) = std::fs::create_dir_all(&shots.dir) {
                cleanup(&dir);
                return Err(HarnessError::Gate {
                    what: format!("cannot create {}: {e}", shots.dir.display()),
                });
            }
            cmd.arg("--shots")
                .arg(shots.frames_arg())
                .arg("--shots-dir")
                .arg(&shots.dir);
        }
        if let Some(shots) = gx_shots.filter(|s| !s.frames.is_empty()) {
            if let Err(e) = std::fs::create_dir_all(&shots.dir) {
                cleanup(&dir);
                return Err(HarnessError::Gate {
                    what: format!("cannot create {}: {e}", shots.dir.display()),
                });
            }
            cmd.arg("--gx-shots")
                .arg(shots.frames_arg())
                .arg("--gx-shots-dir")
                .arg(&shots.dir);
        }

        let status = cmd.output();
        let output = match status {
            Ok(o) => o,
            Err(e) => {
                cleanup(&dir);
                return Err(HarnessError::Gate {
                    what: format!("cannot run {}: {e}", binary.display()),
                });
            }
        };
        if !output.status.success() {
            cleanup(&dir);
            return Err(HarnessError::Gate {
                what: format!(
                    "oracle exited {:?}: {}",
                    output.status.code(),
                    String::from_utf8_lossy(&output.stderr)
                ),
            });
        }

        let text = std::fs::read_to_string(&out_path).map_err(|e| {
            cleanup(&dir);
            HarnessError::Gate {
                what: format!("oracle wrote no trace at {}: {e}", out_path.display()),
            }
        });
        cleanup(&dir);
        let text = text?;
        Trace::parse(&text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn region_set() -> RegionSet {
        RegionSet::parse("# name bucket address size sample\nrng hard 0x021D15A8 4 1\n").unwrap()
    }

    fn script() -> InputScript {
        InputScript::parse("# apricorn input v1\nend 4\n1 down A\n2 up A\n").unwrap()
    }

    #[test]
    fn regions_blob_matches_the_format_contract() {
        let blob = regions_blob(&region_set());
        // count, addr, size, sample, name_len, name
        let expected: Vec<u8> = [1u32, 0x021D_15A8, 4, 1, 3]
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .chain(b"rng".iter().copied())
            .collect();
        assert_eq!(blob, expected);
    }

    #[test]
    fn input_blob_covers_the_script_then_idle() {
        let script = script();
        // 3 frames: down-A at 1, up-A at 2, frame 3 past `end` = idle.
        let blob = input_blob(&script, 3);
        assert_eq!(&blob[..4], &3u32.to_le_bytes());
        // frame 0: idle. frame 1: A (bit 0 of melonDS's keymask layout).
        assert_eq!(&blob[4..12], &[0, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(&blob[12..20], &[1, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(&blob[20..28], &[0, 0, 0, 0, 0, 0, 0, 0]);
    }

    #[test]
    fn probes_blob_pads_args_to_four() {
        let blob = probes_blob(&[ProbeSpec {
            frame: 5,
            entry: 0x0201_FD39, // | Thumb bit
            args: vec![0x1234],
            name: "SetLCRNGSeed".to_string(),
        }])
        .unwrap();
        let expected: Vec<u8> = [1u32, 5, 0x0201_FD39, 1, 0x1234, 0, 0, 0, 12]
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .chain(b"SetLCRNGSeed".iter().copied())
            .collect();
        assert_eq!(blob, expected);
    }

    #[test]
    fn probes_blob_rejects_five_args() {
        let err = probes_blob(&[ProbeSpec {
            frame: 0,
            entry: 0,
            args: vec![1, 2, 3, 4, 5],
            name: "greedy".to_string(),
        }]);
        assert!(matches!(err, Err(HarnessError::Gate { .. })));
    }

    #[test]
    fn shot_frames_parse_items_and_ranges() {
        assert_eq!(parse_shot_frames("120").unwrap(), vec![120]);
        assert_eq!(
            parse_shot_frames("120, 300-303,5").unwrap(),
            vec![120, 300, 301, 302, 303, 5]
        );
        assert!(parse_shot_frames("").is_err());
        assert!(parse_shot_frames("12,").is_err());
        assert!(parse_shot_frames("abc").is_err());
        assert!(parse_shot_frames("10-5").is_err());
    }

    #[test]
    fn shot_request_names_files_and_sorts_the_flag() {
        let shots = ShotRequest {
            frames: vec![300, 12, 300, 7],
            dir: PathBuf::from("shots"),
        };
        assert_eq!(shots.frames_arg(), "7,12,300");
        assert_eq!(
            shots.top_path(7),
            PathBuf::from("shots").join("frame_000007_top.png")
        );
        assert_eq!(
            shots.bottom_path(300),
            PathBuf::from("shots").join("frame_000300_bottom.png")
        );
    }

    #[test]
    fn gx_snapshot_round_trips_planes_and_change_mask() {
        let snapshot = |colors: [u32; 2]| {
            let mut bytes = b"APGX3D1\0".to_vec();
            bytes.extend_from_slice(&2u32.to_le_bytes());
            bytes.extend_from_slice(&1u32.to_le_bytes());
            for (index, color) in colors.into_iter().enumerate() {
                bytes.extend_from_slice(&color.to_le_bytes());
                bytes.extend_from_slice(&(0x100 + index as u32).to_le_bytes());
                bytes.extend_from_slice(&(0x200 + index as u32).to_le_bytes());
            }
            Gx3dSnapshot::parse(&bytes).unwrap()
        };
        let first = snapshot([10, 20]);
        let second = snapshot([10, 21]);
        assert_eq!((first.width(), first.height()), (2, 1));
        assert_eq!(first.colors(), &[10, 20]);
        assert_eq!(first.depths(), &[0x100, 0x101]);
        assert_eq!(first.attributes(), &[0x200, 0x201]);
        assert_eq!(first.color_change_mask(&second).unwrap(), [false, true]);
        assert_eq!(
            first.diff(&second).unwrap(),
            Gx3dDiff {
                color_pixels: 1,
                depth_pixels: 0,
                attribute_pixels: 0,
                coverage_pixels: 0,
            }
        );
        assert!(!first.diff(&snapshot([10, 0x0100_0015])).unwrap().is_exact());
        assert_eq!(
            first
                .diff(&snapshot([10, 0x0100_0015]))
                .unwrap()
                .coverage_pixels,
            1
        );
    }

    #[test]
    fn gx_snapshot_rejects_bad_size() {
        let mut bytes = b"APGX3D1\0".to_vec();
        bytes.extend_from_slice(&256u32.to_le_bytes());
        bytes.extend_from_slice(&192u32.to_le_bytes());
        assert!(Gx3dSnapshot::parse(&bytes).is_err());
        let mut empty = b"APGX3D1\0".to_vec();
        empty.extend_from_slice(&0u32.to_le_bytes());
        empty.extend_from_slice(&192u32.to_le_bytes());
        assert!(Gx3dSnapshot::parse(&empty).is_err());
    }

    #[test]
    fn gx_shot_request_names_files_and_sorts_the_flag() {
        let shots = GxShotRequest {
            frames: vec![9, 7, 9, 8],
            dir: PathBuf::from("gx"),
        };
        assert_eq!(shots.frames_arg(), "7,8,9");
        assert_eq!(
            shots.path(8),
            PathBuf::from("gx").join("frame_000008_gx3d.bin")
        );
    }

    #[test]
    fn empty_input_sha_is_the_empty_script_sha() {
        let run = OracleRun {
            rom: Path::new("rom"),
            regions: &region_set(),
            input: None,
            probes: &[],
            frames: 0,
            rtc: None,
            producer: None,
        };
        assert_eq!(
            run.input_sha1_hex(),
            InputScript {
                rtc: None,
                end: 0,
                events: Vec::new()
            }
            .sha1_hex()
        );
    }
}

//! Corpus case loading and replay: the bridge between a case directory
//! (`regions.conf` + `input.apin` + optional `probes.conf`) and the
//! oracle, with the strict compare against `expected.trace`.
//!
//! A corpus case is the unit the [`apricorn-replay`](../bin/apricorn-replay.rs)
//! binary runs end to end:
//!
//! ```text
//! corpus/boot-idle/
//!   input.apin       # the pinned RTC, the length in frames, the events
//!   regions.conf     # what to hash, how often, which bucket
//!   probes.conf      # (fnprobe cases only) which functions to call when
//!   expected.trace   # the committed oracle output — hashes only
//! ```
//!
//! `expected.trace` is regenerated only by a deliberate, reviewed
//! `--update` (never implicitly): it is the corpus's definition of
//! correct, so a diff against it is only as trustworthy as the care
//! taken in updating it. The committed files carry hashes only — no
//! game content (the ROM stays a local, gitignored dump).
//!
//! The same case replays on the engine ([`Case::run_engine`],
//! `apricorn-replay --engine`): the real `apricorn-core` game under
//! the case's script and regions, its trace compared against the
//! oracle's `expected.trace` through the same comparator. The engine
//! never defines correct — `--update` stays an oracle-only act.

use std::path::{Path, PathBuf};

use crate::HarnessError;
use crate::engine::EngineRun;
use crate::input::InputScript;
use crate::oracle::{GxShotRequest, OracleRun, ProbeSpec, ShotRequest};
use crate::pins::{PinMode, PinTable};
use crate::regions::RegionSet;
use crate::trace::Trace;

/// One corpus case, loaded from its directory.
#[derive(Debug, Clone)]
pub struct Case {
    /// The case's directory (diagnostics and `expected.trace` live here).
    pub dir: PathBuf,
    /// The watched regions.
    pub regions: RegionSet,
    /// The input script — required: it pins the RTC and the length.
    pub script: InputScript,
    /// The probes (fnprobe cases); empty for pure replays.
    pub probes: Vec<ProbeSpec>,
}

impl Case {
    /// Loads `regions.conf`, `input.apin`, and the optional
    /// `probes.conf` from `dir`.
    ///
    /// # Errors
    /// Returns a [`HarnessError::Gate`] naming the file when a required
    /// file is missing or any parse fails; probe entries must name pins
    /// and appear in nondecreasing frame order.
    pub fn load(dir: &Path) -> Result<Self, HarnessError> {
        let gate = |what: String| HarnessError::Gate { what };
        let read = |name: &str| -> Result<String, HarnessError> {
            std::fs::read_to_string(dir.join(name))
                .map_err(|e| gate(format!("case {}: cannot read {name}: {e}", dir.display())))
        };

        let regions = RegionSet::parse(&read("regions.conf")?)
            .map_err(|e| gate(format!("case {}: regions.conf: {e}", dir.display())))?;
        let script = InputScript::parse(&read("input.apin")?)
            .map_err(|e| gate(format!("case {}: input.apin: {e}", dir.display())))?;
        let probes = match std::fs::read_to_string(dir.join("probes.conf")) {
            Ok(text) => probes_parse(&text)
                .map_err(|e| gate(format!("case {}: probes.conf: {e}", dir.display())))?,
            Err(_) => Vec::new(),
        };

        Ok(Self {
            dir: dir.to_path_buf(),
            regions,
            script,
            probes,
        })
    }

    /// The replay length in frames (`input.apin`'s `end`).
    #[must_use]
    pub fn frames(&self) -> u32 {
        self.script.end
    }

    /// The pinned RTC (`input.apin`'s `rtc`), if the case sets one.
    #[must_use]
    pub fn rtc(&self) -> Option<&str> {
        self.script.rtc.as_deref()
    }

    /// The case's oracle invocation on `rom`.
    fn oracle_run<'a>(&'a self, rom: &'a Path) -> OracleRun<'a> {
        OracleRun {
            rom,
            regions: &self.regions,
            input: Some(&self.script),
            probes: &self.probes,
            frames: self.frames(),
            rtc: self.rtc(),
            producer: None,
        }
    }

    /// Replays the case on the oracle and parses the trace it produces.
    ///
    /// # Errors
    /// Propagates [`crate::oracle::OracleRun::run`]'s errors (binary
    /// missing, nonzero exit, malformed trace).
    pub fn run_oracle(&self, rom: &Path) -> Result<Trace, HarnessError> {
        self.oracle_run(rom).run()
    }

    /// [`run_oracle`](Self::run_oracle), also writing the screenshots
    /// `shots` asks for — the same trace, plus PNGs of both LCDs at the
    /// listed frames for visual review against engine renders.
    ///
    /// # Errors
    /// Propagates [`crate::oracle::OracleRun::run_with_shots`]'s errors.
    pub fn run_oracle_with_shots(
        &self,
        rom: &Path,
        shots: &ShotRequest,
    ) -> Result<Trace, HarnessError> {
        self.oracle_run(rom).run_with_shots(shots)
    }

    /// [`run_oracle`](Self::run_oracle), also writing the raw software
    /// 3D planes requested by `shots` before 2D composition.
    ///
    /// # Errors
    /// Propagates [`crate::oracle::OracleRun::run_with_gx_shots`]'s errors.
    pub fn run_oracle_with_gx_shots(
        &self,
        rom: &Path,
        shots: &GxShotRequest,
    ) -> Result<Trace, HarnessError> {
        self.oracle_run(rom).run_with_gx_shots(shots)
    }

    /// Runs the oracle while writing both composited and raw 3D artifacts.
    ///
    /// # Errors
    /// Propagates [`crate::oracle::OracleRun::run_with_artifacts`]'s errors.
    pub fn run_oracle_with_artifacts(
        &self,
        rom: &Path,
        shots: &ShotRequest,
        gx_shots: &GxShotRequest,
    ) -> Result<Trace, HarnessError> {
        self.oracle_run(rom).run_with_artifacts(shots, gx_shots)
    }

    /// Replays the case on the engine — the real `apricorn-core` game
    /// through [`EngineRun`], with a blank card — and returns its
    /// trace, header gates computed as the oracle computes them so
    /// the pair compares.
    ///
    /// Probes (`probes.conf`) are an oracle/arm-runner notion (pinned
    /// addresses to call); a case that carries them still replays its
    /// frames here, without `C` records.
    ///
    /// # Errors
    /// Propagates [`EngineRun::run`]'s errors: a region the engine
    /// cannot serve (named), an unparsable `rtc`, an unreadable ROM.
    pub fn run_engine(&self, rom: &Path) -> Result<Trace, HarnessError> {
        EngineRun {
            rom,
            regions: &self.regions,
            script: &self.script,
            save: None,
            frames: None,
            producer: None,
        }
        .run()
        .map(|output| output.trace)
    }

    /// The case's committed expected trace, if present.
    ///
    /// # Errors
    /// Returns a [`HarnessError::Gate`] when the file exists but does
    /// not parse.
    pub fn expected(&self) -> Result<Option<Trace>, HarnessError> {
        let path = self.dir.join("expected.trace");
        let Ok(text) = std::fs::read_to_string(&path) else {
            return Ok(None);
        };
        Trace::parse(&text)
            .map(Some)
            .map_err(|e| HarnessError::Gate {
                what: format!("case {}: expected.trace: {e}", self.dir.display()),
            })
    }

    /// Writes `trace` as the case's new expected trace (the deliberate,
    /// reviewed `--update` path).
    ///
    /// # Errors
    /// Returns a [`HarnessError::Gate`] when the write fails.
    pub fn write_expected(&self, trace: &Trace) -> Result<(), HarnessError> {
        let path = self.dir.join("expected.trace");
        std::fs::write(&path, trace.to_string()).map_err(|e| HarnessError::Gate {
            what: format!("cannot write {}: {e}", path.display()),
        })
    }
}

/// Parses `probes.conf`: one probe per line, `frame fn [args…]` —
/// `fn` must be a (code) pin, whose table entry supplies the address
/// and the Thumb bit; args are hex (`0x…`) or decimal. Blank lines and
/// `#` comments are skipped. Probes run in file order, so frames must
/// not decrease (the oracle consumes them grouped by frame boundary).
///
/// # Errors
/// Returns a [`HarnessError::Syntax`] naming the line of any malformed
/// entry, unknown function, or out-of-order frame.
pub fn probes_parse(text: &str) -> Result<Vec<ProbeSpec>, HarnessError> {
    let table = PinTable::arm9();
    let mut probes = Vec::<ProbeSpec>::new();
    for (idx, raw) in text.lines().enumerate() {
        let line_no = idx + 1;
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let bad = |what: &str| HarnessError::Syntax {
            line: line_no,
            what: what.to_string(),
        };

        let mut fields = line.split_whitespace();
        let frame: u32 = fields
            .next()
            .ok_or_else(|| bad("expected: frame fn [args…]"))?
            .parse()
            .map_err(|_| bad("frame must be a decimal"))?;
        let name = fields
            .next()
            .ok_or_else(|| bad("expected: frame fn [args…]"))?
            .to_string();
        let pin = table
            .get(&name)
            .ok_or_else(|| bad(&format!("unknown pin {name}")))?;
        if pin.mode == PinMode::Data {
            return Err(bad(&format!("{name} is a data pin, not callable")));
        }
        let entry = pin.address | u32::from(pin.mode == PinMode::Thumb);
        let mut args = Vec::new();
        for arg in fields {
            let value = if let Some(hex) = arg.strip_prefix("0x") {
                u32::from_str_radix(hex, 16).map_err(|_| bad(&format!("arg {arg}: bad hex")))?
            } else {
                arg.parse()
                    .map_err(|_| bad(&format!("arg {arg}: bad decimal")))?
            };
            args.push(value);
        }
        if args.len() > 4 {
            return Err(bad("more than 4 args"));
        }
        if let Some(last) = probes.last()
            && last.frame > frame
        {
            return Err(bad("frames must not decrease (probes run in file order)"));
        }
        probes.push(ProbeSpec {
            frame,
            entry,
            args,
            name,
        });
    }
    Ok(probes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probes_parse_reads_frame_fn_args() {
        let probes =
            probes_parse("# frame fn args...\n20 SetLCRNGSeed 0x1234\n21 LCRandom\n").unwrap();
        assert_eq!(probes.len(), 2);
        assert_eq!(probes[0].frame, 20);
        assert_eq!(probes[0].entry & 1, 1, "Thumb pin must set the entry bit");
        assert_eq!(probes[0].args, vec![0x1234]);
        assert_eq!(probes[1].args, Vec::<u32>::new());
    }

    #[test]
    fn probes_parse_rejects_disorder_and_unknowns() {
        assert!(probes_parse("20 LCRandom\n19 LCRandom\n").is_err());
        assert!(probes_parse("20 NoSuchFunction\n").is_err());
        // Data pins are not callable.
        assert!(probes_parse("20 sLCRNG_State\n").is_err());
        assert!(probes_parse("20 LCRandom 1 2 3 4 5\n").is_err());
    }
}

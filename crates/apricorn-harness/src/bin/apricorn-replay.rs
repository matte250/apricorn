//! `apricorn-replay` — run a corpus case on the oracle (or the
//! engine), diff against the committed expected trace.
//!
//! ```text
//! usage: apricorn-replay [--update] [--engine] [--rom ROM]
//!                        [--shots FRAMES --shots-dir DIR] <case-dir>
//!                        [--gx-shots FRAMES --gx-shots-dir DIR]
//!                        [--hold BUTTON FIRST-LAST]
//! ```
//!
//! Loads the case (`regions.conf` + `input.apin` + optional
//! `probes.conf`), replays it on the oracle, and compares the produced
//! trace against the case's `expected.trace` with the case's own
//! buckets. Exit code 0 = EQUIVALENT, 1 = diverged, 64 = usage or gate
//! error — the same conventions as `apricorn-diff`.
//!
//! `--engine` replays the case on the real `apricorn-core` game
//! instead (`apricorn_harness::engine`) and compares *that* trace
//! against the oracle's `expected.trace` — the engine-vs-oracle
//! verdict, printed as EQUIVALENT or the first divergence's frame,
//! region, and both hashes.
//!
//! `--update` regenerates `expected.trace` from an oracle run instead
//! of comparing: a deliberate, reviewed act (the corpus's definition
//! of correct must never change implicitly) — and an oracle-only one:
//! `--update --engine` is refused, the engine never defines correct.
//! `--rom` overrides the default dump (`hg_usa.nds` at the repo
//! root); the ROM is a local, gitignored file.
//!
//! `--shots FRAMES --shots-dir DIR` additionally writes both LCDs as
//! PNGs at the end of the listed frames (`120,300-305`: items or
//! inclusive ranges) into `DIR` — `frame_NNNNNN_top.png` /
//! `_bottom.png` — without changing the trace or the verdict. The
//! PNGs are review artifacts (ground truth for engine renders); keep
//! them under `out/`, never in the corpus. Shots are an oracle
//! feature: `--shots --engine` is refused (`apricorn-run` dumps the
//! engine's frames).
//!
//! `--gx-shots FRAMES --gx-shots-dir DIR` writes the raw software-3D
//! colour, depth, and attribute planes before 2D composition.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use apricorn_harness::diff;
use apricorn_harness::input::{Action, InputEvent, button_mask};
use apricorn_harness::oracle::{GxShotRequest, ShotRequest, parse_shot_frames};
use apricorn_harness::replay::Case;

const USAGE: &str = "usage: apricorn-replay [--update] [--engine] [--rom ROM] [--shots FRAMES --shots-dir DIR] [--gx-shots FRAMES --gx-shots-dir DIR] [--hold BUTTON FIRST-LAST] <case-dir>";

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);

    // Flags may come before or after the case dir (the wrapper
    // scripts append theirs); exactly one positional is the case.
    let mut update = false;
    let mut engine = false;
    let mut rom: Option<PathBuf> = None;
    let mut shot_frames: Option<String> = None;
    let mut shots_dir: Option<PathBuf> = None;
    let mut gx_shot_frames: Option<String> = None;
    let mut gx_shots_dir: Option<PathBuf> = None;
    let mut holds = Vec::new();
    let mut case_dir: Option<String> = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--update" => update = true,
            "--engine" => engine = true,
            "--rom" => {
                let Some(path) = args.next() else {
                    eprintln!("{USAGE}\n  --rom needs a path");
                    return ExitCode::from(64);
                };
                rom = Some(PathBuf::from(path));
            }
            "--shots" => {
                let Some(frames) = args.next() else {
                    eprintln!("{USAGE}\n  --shots needs a frame list (120,300-305)");
                    return ExitCode::from(64);
                };
                shot_frames = Some(frames);
            }
            "--shots-dir" => {
                let Some(dir) = args.next() else {
                    eprintln!("{USAGE}\n  --shots-dir needs a path");
                    return ExitCode::from(64);
                };
                shots_dir = Some(PathBuf::from(dir));
            }
            "--gx-shots" => {
                let Some(frames) = args.next() else {
                    eprintln!("{USAGE}\n  --gx-shots needs a frame list (120,300-305)");
                    return ExitCode::from(64);
                };
                gx_shot_frames = Some(frames);
            }
            "--gx-shots-dir" => {
                let Some(dir) = args.next() else {
                    eprintln!("{USAGE}\n  --gx-shots-dir needs a path");
                    return ExitCode::from(64);
                };
                gx_shots_dir = Some(PathBuf::from(dir));
            }
            "--hold" => {
                let (Some(button), Some(range)) = (args.next(), args.next()) else {
                    eprintln!("{USAGE}\n  --hold needs BUTTON and FIRST-LAST");
                    return ExitCode::from(64);
                };
                let Some(mask) = button_mask(&button) else {
                    eprintln!("{USAGE}\n  --hold names an unknown button: {button}");
                    return ExitCode::from(64);
                };
                let Some((first, last)) = range.split_once('-') else {
                    eprintln!("{USAGE}\n  --hold range must be FIRST-LAST");
                    return ExitCode::from(64);
                };
                let Ok(first) = first.parse::<u32>() else {
                    eprintln!("{USAGE}\n  --hold first frame is not a number");
                    return ExitCode::from(64);
                };
                let Ok(last) = last.parse::<u32>() else {
                    eprintln!("{USAGE}\n  --hold last frame is not a number");
                    return ExitCode::from(64);
                };
                if first >= last {
                    eprintln!("{USAGE}\n  --hold requires FIRST < LAST");
                    return ExitCode::from(64);
                }
                holds.push((mask, first, last));
            }
            _ if arg.starts_with("--") => {
                eprintln!("{USAGE}\n  unknown flag {arg}");
                return ExitCode::from(64);
            }
            _ if case_dir.is_none() => case_dir = Some(arg),
            _ => {
                eprintln!("{USAGE}\n  exactly one case dir");
                return ExitCode::from(64);
            }
        }
    }
    let Some(case_dir) = case_dir else {
        eprintln!("{USAGE}");
        return ExitCode::from(64);
    };
    if update && engine {
        eprintln!(
            "{USAGE}\n  --update regenerates the oracle baseline; the engine never defines correct (drop --engine)"
        );
        return ExitCode::from(64);
    }
    let shots = match (shot_frames, shots_dir) {
        (None, None) => None,
        (Some(frames), Some(dir)) => match parse_shot_frames(&frames) {
            Ok(frames) => Some(ShotRequest { frames, dir }),
            Err(e) => {
                eprintln!("{USAGE}\n  {e}");
                return ExitCode::from(64);
            }
        },
        _ => {
            eprintln!("{USAGE}\n  --shots and --shots-dir go together");
            return ExitCode::from(64);
        }
    };
    let gx_shots = match (gx_shot_frames, gx_shots_dir) {
        (None, None) => None,
        (Some(frames), Some(dir)) => match parse_shot_frames(&frames) {
            Ok(frames) => Some(GxShotRequest { frames, dir }),
            Err(e) => {
                eprintln!("{USAGE}\n  {e}");
                return ExitCode::from(64);
            }
        },
        _ => {
            eprintln!("{USAGE}\n  --gx-shots and --gx-shots-dir go together");
            return ExitCode::from(64);
        }
    };
    if engine && (shots.is_some() || gx_shots.is_some()) {
        eprintln!("{USAGE}\n  screenshot flags are oracle features (drop --engine)");
        return ExitCode::from(64);
    }

    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let rom = rom.unwrap_or_else(|| repo.join("hg_usa.nds"));

    match run(
        &case_dir,
        &rom,
        update,
        engine,
        shots.as_ref(),
        gx_shots.as_ref(),
        &holds,
    ) {
        Ok(code) => code,
        Err(message) => {
            eprintln!("apricorn-replay: {message}");
            ExitCode::from(64)
        }
    }
}

fn run(
    case_dir: &str,
    rom: &Path,
    update: bool,
    engine: bool,
    shots: Option<&ShotRequest>,
    gx_shots: Option<&GxShotRequest>,
    holds: &[(u16, u32, u32)],
) -> Result<ExitCode, String> {
    let mut case = Case::load(Path::new(case_dir)).map_err(|e| e.to_string())?;
    for &(mask, first, last) in holds {
        if last >= case.frames() {
            return Err(format!("--hold ends at frame {last}, outside end {}", case.frames()));
        }
        case.script.events.push(InputEvent {
            frame: first,
            action: Action::Down(mask),
        });
        case.script.events.push(InputEvent {
            frame: last,
            action: Action::Up(mask),
        });
    }
    case.script.events.sort_by_key(|event| event.frame);

    let actual = if engine {
        case.run_engine(rom).map_err(|e| e.to_string())?
    } else {
        match (shots, gx_shots) {
            (Some(shots), Some(gx_shots)) => case
                .run_oracle_with_artifacts(rom, shots, gx_shots)
                .map_err(|e| e.to_string())?,
            (Some(shots), None) => {
                let trace = case
                    .run_oracle_with_shots(rom, shots)
                    .map_err(|e| e.to_string())?;
                let written = shots.frames.iter().filter(|&&f| f < case.frames()).count();
                println!(
                    "wrote {written} frame(s) of screenshots to {}",
                    shots.dir.display()
                );
                trace
            }
            (None, Some(gx_shots)) => {
                let trace = case
                    .run_oracle_with_gx_shots(rom, gx_shots)
                    .map_err(|e| e.to_string())?;
                println!("wrote raw GX snapshots to {}", gx_shots.dir.display());
                trace
            }
            (None, None) => case.run_oracle(rom).map_err(|e| e.to_string())?,
        }
    };

    if update {
        case.write_expected(&actual).map_err(|e| e.to_string())?;
        println!(
            "updated {} ({} records)",
            case.dir.join("expected.trace").display(),
            actual.records.len()
        );
        return Ok(ExitCode::SUCCESS);
    }

    if !holds.is_empty() {
        println!("captured artifacts with {} temporary held-input interval(s)", holds.len());
        return Ok(ExitCode::SUCCESS);
    }

    let expected = case.expected().map_err(|e| e.to_string())?;
    let Some(expected) = expected else {
        return Err(format!(
            "case {} has no expected.trace (run with --update to create it)",
            case.dir.display()
        ));
    };

    println!(
        "comparing {} against {} ({} frames)",
        actual.header.producer, expected.header.producer, expected.header.frames
    );
    let report =
        diff::compare(&expected, &actual, Some(&case.regions)).map_err(|e| e.to_string())?;
    println!("{report}");
    Ok(match report.verdict {
        apricorn_harness::Verdict::Equivalent => ExitCode::SUCCESS,
        apricorn_harness::Verdict::Diverged { .. } => ExitCode::FAILURE,
    })
}

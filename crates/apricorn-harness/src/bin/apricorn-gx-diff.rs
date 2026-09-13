//! Exact comparator for raw `APGX3D1` renderer snapshots.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use apricorn_harness::oracle::Gx3dSnapshot;

const USAGE: &str = "usage: apricorn-gx-diff EXPECTED ACTUAL [--previous EXPECTED_PREVIOUS ACTUAL_PREVIOUS]\n       apricorn-gx-diff --sequence PAIRS.tsv";

fn sequence_pairs(manifest: &Path, contents: &str) -> Result<Vec<(PathBuf, PathBuf)>, String> {
    let base = manifest.parent().unwrap_or(Path::new("."));
    let mut pairs = Vec::new();
    for (line, value) in contents.lines().enumerate() {
        if value.trim().is_empty() || value.trim_start().starts_with('#') {
            continue;
        }
        let columns = value.split('\t').map(str::trim).collect::<Vec<_>>();
        if columns.len() != 2 || columns.iter().any(|column| column.is_empty()) {
            return Err(format!(
                "{}:{}: expected two tab-separated paths",
                manifest.display(),
                line + 1
            ));
        }
        pairs.push((base.join(columns[0]), base.join(columns[1])));
    }
    if pairs.len() < 2 {
        return Err("a motion sequence requires at least two frame pairs".into());
    }
    Ok(pairs)
}

fn change_difference(
    previous: (&Gx3dSnapshot, &Gx3dSnapshot),
    current: (&Gx3dSnapshot, &Gx3dSnapshot),
) -> Result<usize, String> {
    let expected = previous
        .0
        .color_change_mask(current.0)
        .map_err(|error| error.to_string())?;
    let actual = previous
        .1
        .color_change_mask(current.1)
        .map_err(|error| error.to_string())?;
    if expected.len() != actual.len() {
        return Err("expected and actual change-mask dimensions differ".into());
    }
    Ok(expected.iter().zip(&actual).filter(|(a, b)| a != b).count())
}

fn compare_sequence(manifest: &Path) -> Result<bool, String> {
    let contents = std::fs::read_to_string(manifest)
        .map_err(|error| format!("{}: {error}", manifest.display()))?;
    let pairs = sequence_pairs(manifest, &contents)?;
    let mut previous: Option<(Gx3dSnapshot, Gx3dSnapshot)> = None;
    let mut exact = true;
    for (index, (expected_path, actual_path)) in pairs.iter().enumerate() {
        let expected = Gx3dSnapshot::read(expected_path).map_err(|error| error.to_string())?;
        let actual = Gx3dSnapshot::read(actual_path).map_err(|error| error.to_string())?;
        let diff = expected.diff(&actual).map_err(|error| error.to_string())?;
        let changes = if let Some((e, a)) = &previous {
            change_difference((e, a), (&expected, &actual))?
        } else {
            0
        };
        println!(
            "frame={index} color={} depth={} attributes={} coverage={} change-mask={changes}",
            diff.color_pixels, diff.depth_pixels, diff.attribute_pixels, diff.coverage_pixels
        );
        exact &= diff.is_exact() && changes == 0;
        previous = Some((expected, actual));
    }
    println!("frames={} transitions={}", pairs.len(), pairs.len() - 1);
    Ok(exact)
}

fn read(path: &str) -> Result<Gx3dSnapshot, String> {
    Gx3dSnapshot::read(Path::new(path)).map_err(|error| format!("{path}: {error}"))
}

fn main() -> ExitCode {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if args.len() != 2 && !(args.len() == 5 && args[2] == "--previous") {
        eprintln!("{USAGE}");
        return ExitCode::from(64);
    }

    let result = (|| {
        if args[0] == "--sequence" {
            if args.len() != 2 {
                return Err(USAGE.to_string());
            }
            return compare_sequence(Path::new(&args[1]));
        }
        let expected = read(&args[0])?;
        let actual = read(&args[1])?;
        let diff = expected.diff(&actual).map_err(|error| error.to_string())?;
        println!(
            "color={} depth={} attributes={} coverage={}",
            diff.color_pixels, diff.depth_pixels, diff.attribute_pixels, diff.coverage_pixels
        );

        let mut change_mask_pixels = 0;
        if args.len() == 5 {
            let expected_previous = read(&args[3])?;
            let actual_previous = read(&args[4])?;
            change_mask_pixels =
                change_difference((&expected_previous, &actual_previous), (&expected, &actual))?;
            println!("change-mask={change_mask_pixels}");
        }
        Ok::<bool, String>(diff.is_exact() && change_mask_pixels == 0)
    })();

    match result {
        Ok(true) => {
            println!("EXACT");
            ExitCode::SUCCESS
        }
        Ok(false) => {
            println!("DIFFERENT");
            ExitCode::from(1)
        }
        Err(error) => {
            eprintln!("{error}");
            ExitCode::from(64)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_is_relative_and_preserves_order_and_spaces() {
        let pairs = sequence_pairs(
            Path::new("review/pairs.tsv"),
            "# expected\tactual\n\nref/a 1.bin\tengine/0.bin\nref/a 1.bin\tengine/1.bin\n",
        )
        .unwrap();
        assert_eq!(pairs[0].0, Path::new("review/ref/a 1.bin"));
        assert_eq!(pairs[1].1, Path::new("review/engine/1.bin"));
        assert_eq!(
            pairs[0].0, pairs[1].0,
            "repeated display frames must not be deduplicated"
        );
    }

    #[test]
    fn malformed_or_empty_sequences_cannot_pass() {
        for contents in [
            "",
            "# only comments",
            "a\tb",
            "a b\nc d",
            "a\tb\tc\nd\te",
            "\tb\nc\td",
        ] {
            assert!(sequence_pairs(Path::new("pairs.tsv"), contents).is_err());
        }
    }
}

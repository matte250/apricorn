//! ROM-gated goldens of the live field through the whole pipeline
//! (`FieldSystem` → `FieldFrame` → BG0 → compositor), SHA-1 only: the
//! bedroom mid-step, house 1F on arrival, New Bark Town on arrival.
//! `APRICORN_RENDER_OUT` writes the review PNGs
//! (`field-system-*.png`); no pixels are committed.

use apricorn_core::assets::AssetStore;
use apricorn_core::field::map_object::Direction;
use apricorn_core::field::system::{FieldPhase, FieldSystem, Location};
use apricorn_core::input::{Input, Keys, key};
use sha1::{Digest, Sha1};
use std::path::Path;

const ROM_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../hg_usa.nds");

/// Engine-A hashes: the bedroom four ticks into a step south (the
/// player mid-tile, the camera with him), house 1F at the first fully
/// faded-in tick after the stairs, New Bark Town at the first fully
/// faded-in tick after the front door.
///
/// Preserve main's historical goldens until replacement frames pass the
/// exact oracle gate. The renderer-parity draft intentionally fails these.
const BEDROOM_MID_STEP: &str = "0867fcf6cf096f3b1aae90cf6b8ad00e9851a213";
const HOUSE_1F_ARRIVAL: &str = "99fe2418595825c63b2e6ed2ac774fd67fb69622";
const NEW_BARK_ARRIVAL: &str = "702d762517b6563eab476d612fc48ac2bd327982";

fn open_rom() -> Option<AssetStore> {
    let path = Path::new(ROM_PATH);
    if !path.exists() {
        eprintln!("skipping: no ROM at {}", path.display());
        return None;
    }
    Some(AssetStore::open(path).expect("the pinned ROM opens"))
}

fn sha1(pixels: &[[u8; 4]]) -> String {
    let mut hasher = Sha1::new();
    hasher.update(pixels.as_flattened());
    format!("{:x}", hasher.finalize())
}

fn write_png(name: &str, pixels: &[[u8; 4]]) {
    let Ok(dir) = std::env::var("APRICORN_RENDER_OUT") else {
        return;
    };
    std::fs::create_dir_all(&dir).unwrap();
    let file = std::fs::File::create(Path::new(&dir).join(name)).unwrap();
    let mut enc = png::Encoder::new(file, 256, 192);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header()
        .unwrap()
        .write_image_data(pixels.as_flattened())
        .unwrap();
}

fn held(keys: u16) -> Input {
    Input {
        keys: Keys(keys),
        touch: None,
    }
}

fn render(field: &FieldSystem, store: &AssetStore, name: &str) -> String {
    let [main, sub] = apricorn_gfx::render(field.frame(), store);
    let pixels = main.as_rgba();
    write_png(&format!("{name}.png"), pixels);
    assert!(
        sub.as_rgba().iter().all(|p| p[..3] == [0, 0, 0]),
        "the touch screen stays black for now"
    );
    sha1(pixels)
}

fn write_raw_field(field: &FieldSystem, name: &str) {
    let Some(field_frame) = field.frame().main.field.as_ref() else {
        return;
    };
    let view = apricorn_gfx::field::scene_view(field_frame);
    let mut buffer = apricorn_gfx::Gx3dBuffer::new();
    let registers = apricorn_gfx::field::field_registers(&view);
    apricorn_gfx::field::render_view_gx(&view, &registers, &mut buffer);
    write_gx_snapshot(name.trim_end_matches(".png"), &buffer);
    let uncovered: Vec<_> = buffer
        .colors()
        .iter()
        .enumerate()
        .filter_map(|(index, pixel)| (pixel[3] == 0).then_some((index % 256, index / 256)))
        .collect();
    if std::env::var("APRICORN_RENDER_OUT").is_ok() {
        let pixels: Vec<[u8; 4]> = buffer
            .colors()
            .iter()
            .map(|&pixel| {
                if pixel[3] == 0 {
                    [255, 0, 255, 255]
                } else {
                    pixel
                }
            })
            .collect();
        write_png(name, &pixels);
    }
    assert!(
        uncovered.is_empty(),
        "New Bark 3D plane exposed clear-pixel cracks at {uncovered:?}"
    );
}

fn write_gx_snapshot(name: &str, buffer: &apricorn_gfx::Gx3dBuffer) {
    let Ok(dir) = std::env::var("APRICORN_RENDER_OUT") else {
        return;
    };
    let colors = buffer.native_colors();
    let attributes = buffer.native_attributes();
    let mut bytes = Vec::with_capacity(16 + colors.len() * 12);
    bytes.extend_from_slice(b"APGX3D1\0");
    bytes.extend_from_slice(&256u32.to_le_bytes());
    bytes.extend_from_slice(&192u32.to_le_bytes());
    for ((color, depth), attribute) in colors
        .into_iter()
        .zip(buffer.depths().iter().copied())
        .zip(attributes)
    {
        bytes.extend_from_slice(&color.to_le_bytes());
        bytes.extend_from_slice(&depth.to_le_bytes());
        bytes.extend_from_slice(&attribute.to_le_bytes());
    }
    std::fs::write(Path::new(&dir).join(format!("{name}.gx3d.bin")), bytes).unwrap();
}

fn write_field_snapshot(field: &FieldSystem, name: &str) {
    let Some(field_frame) = field.frame().main.field.as_ref() else {
        return;
    };
    let view = apricorn_gfx::field::scene_view(field_frame);
    let mut buffer = apricorn_gfx::Gx3dBuffer::new();
    let registers = apricorn_gfx::field::field_registers(&view);
    apricorn_gfx::field::render_view_gx(&view, &registers, &mut buffer);
    write_gx_snapshot(name, &buffer);
}

fn run_until_running(field: &mut FieldSystem, store: &AssetStore) {
    while field.phase() != FieldPhase::Running {
        field.tick(Input::default(), store);
    }
}

#[test]
fn bedroom_mid_step_golden() {
    let Some(store) = open_rom() else {
        return;
    };
    let mut field = FieldSystem::new_game_at(&store, 0, 9 * 60 * 60).unwrap();
    run_until_running(&mut field, &store);
    // Four ticks of DOWN: the position vector half a tile south; the
    // frame shows the previous tick's position (three steps) and the
    // walk frame.
    for _ in 0..4 {
        field.tick(held(key::DOWN), &store);
    }
    let hash = render(&field, &store, "field-system-bedroom-mid-step");
    assert_eq!(hash, BEDROOM_MID_STEP, "bedroom mid-step engine A hash");
}

#[test]
fn bedroom_walk_has_a_pinned_contiguous_raster_sequence() {
    let Some(store) = open_rom() else {
        return;
    };
    let mut field = FieldSystem::new_game_at(&store, 0, 9 * 60 * 60).unwrap();
    run_until_running(&mut field, &store);
    let mut actual = Vec::new();
    actual.push(render(&field, &store, "field-motion-00"));
    write_field_snapshot(&field, "field-motion-00");
    for frame in 1..=9 {
        field.tick(held(key::DOWN), &store);
        actual.push(render(&field, &store, &format!("field-motion-{frame:02}")));
        write_field_snapshot(&field, &format!("field-motion-{frame:02}"));
    }
    // Historical diagnostic sequence from the earlier local raster pass,
    // not an oracle-approved golden and not used to claim DS equivalence.
    let expected = [
        "41c3f0a5c362040dad0aece4a4aef6b48d58c99c",
        "41c3f0a5c362040dad0aece4a4aef6b48d58c99c",
        "91fc0d41a7f3eaf7ef1551891bf37ef10f013583",
        "0218152a3d5a6643fcd656efc7ba7c213c42d5d0",
        "ccb8b7d812a736b38cd1f4d653ff019fa68e6359",
        "c413709d06b17a8fe3b521365753a4004e94c460",
        "7f9733568fcaa71c971c6216421f0af10fb2f073",
        "5d392fca5c110792bb29e1b9bb06fd49ea23ccf1",
        "528e2e3ae71764eee5addf9aff00fa346085f8e0",
        "ed658a77d642b3781f39f931d262908cea8f01ba",
    ];
    assert_eq!(actual, expected, "the complete step must stay frame-exact");
}

#[test]
fn house_1f_arrival_golden() {
    let Some(store) = open_rom() else {
        return;
    };
    // The stairs' arrival: one tile into the wall west of 1F's stairs,
    // walking out — ticked through the transition's fade-in to the
    // first fully bright frame.
    let mut field = FieldSystem::new_game_at(&store, 0, 9 * 60 * 60).unwrap();
    run_until_running(&mut field, &store);
    for _ in 0..27 {
        field.tick(held(key::LEFT), &store);
    }
    field.tick(Input::default(), &store);
    for _ in 0..19 {
        field.tick(held(key::UP), &store);
    }
    field.tick(Input::default(), &store);
    for _ in 0..4 {
        field.tick(held(key::LEFT), &store);
    }
    assert_eq!(field.phase(), FieldPhase::Transition);
    let mut ticks = 0;
    while field.frame().main.brightness != apricorn_core::frame::MasterBrightness::default()
        || field.scene().map_id != 63
    {
        field.tick(Input::default(), &store);
        ticks += 1;
        assert!(ticks < 200);
    }
    assert_eq!(field.scene().map_id, 63);
    assert_eq!(field.avatar().facing(), Direction::East);
    let hash = render(&field, &store, "field-system-house-1f-arrival");
    assert_eq!(hash, HOUSE_1F_ARRIVAL, "house 1F arrival engine A hash");
}

#[test]
fn new_bark_arrival_golden() {
    let Some(store) = open_rom() else {
        return;
    };
    let mut field = FieldSystem::enter_at(
        &store,
        Location::new(63, 1, 0, 0, Direction::East),
        0,
        9 * 60 * 60,
    )
    .unwrap();
    run_until_running(&mut field, &store);
    for _ in 0..60 {
        field.tick(held(key::DOWN), &store);
    }
    assert_eq!(field.phase(), FieldPhase::Transition);
    let mut ticks = 0;
    while field.frame().main.brightness != apricorn_core::frame::MasterBrightness::default()
        || field.scene().map_id != 60
    {
        field.tick(Input::default(), &store);
        ticks += 1;
        assert!(ticks < 200);
    }
    assert_eq!(field.scene().map_id, 60);
    let hash = render(&field, &store, "field-system-new-bark-arrival");
    // A few more frames for review: the walk off the door and a turn.
    for _ in 0..12 {
        field.tick(Input::default(), &store);
    }
    render(&field, &store, "field-system-new-bark-standing");
    write_raw_field(&field, "field-system-new-bark-standing-raw.png");
    for _ in 0..30 {
        field.tick(held(key::RIGHT), &store);
    }
    render(&field, &store, "field-system-new-bark-east");
    write_raw_field(&field, "field-system-new-bark-east-raw.png");
    field.tick(Input::default(), &store);
    for _ in 0..120 {
        field.tick(held(key::LEFT), &store);
    }
    render(&field, &store, "field-system-new-bark-west");
    write_raw_field(&field, "field-system-new-bark-west-raw.png");
    assert_eq!(
        hash, NEW_BARK_ARRIVAL,
        "New Bark Town arrival engine A hash"
    );
}

/// Review sequence, not an oracle-approved golden. Keep every sub-tile
/// camera position so furniture-edge and colour changes can be inspected.
#[test]
fn house_furniture_contiguous_review_sequence() {
    let Some(store) = open_rom() else {
        return;
    };
    let mut field = FieldSystem::enter_at(
        &store,
        Location::new(63, -1, 5, 6, Direction::North),
        0,
        9 * 60 * 60,
    )
    .unwrap();
    run_until_running(&mut field, &store);
    let mut targets = std::collections::BTreeSet::new();
    for frame in 0..24 {
        let name = format!("field-house-furniture-{frame:02}");
        let first = render(&field, &store, &name);
        let [second, _] = apricorn_gfx::render(field.frame(), &store);
        assert_eq!(
            first,
            sha1(second.as_rgba()),
            "same frame must render identically"
        );
        write_field_snapshot(&field, &name);
        targets.insert(field.frame().main.field.as_ref().unwrap().camera_target);
        assert_eq!(field.scene().map_id, 63);
        field.tick(held(if frame < 12 { key::UP } else { key::DOWN }), &store);
    }
    assert!(
        targets.len() >= 8,
        "review must actually move the camera, not capture one pose repeatedly"
    );
}

/// Run explicitly with --release --ignored; asset loading and simulation
/// are outside the timed region. No snapshots or disk I/O are timed.
#[test]
#[ignore = "release-only New Bark CPU raster benchmark"]
fn new_bark_release_frame_budget() {
    assert!(!cfg!(debug_assertions), "run this benchmark with --release");
    let Some(store) = open_rom() else {
        return;
    };
    let mut field = FieldSystem::enter_at(
        &store,
        Location::new(60, -1, 695, 397, Direction::West),
        0,
        9 * 60 * 60,
    )
    .unwrap();
    run_until_running(&mut field, &store);
    let mut buffer = apricorn_gfx::Gx3dBuffer::new();
    let mut samples = Vec::new();
    for tick in 0..144 {
        field.tick(held(if tick < 72 { key::LEFT } else { key::RIGHT }), &store);
        let view = apricorn_gfx::field::scene_view(field.frame().main.field.as_ref().unwrap());
        let registers = apricorn_gfx::field::field_registers(&view);
        let start = std::time::Instant::now();
        apricorn_gfx::field::render_view_gx(std::hint::black_box(&view), &registers, &mut buffer);
        let elapsed = start.elapsed();
        std::hint::black_box(buffer.colors());
        if tick >= 24 {
            samples.push(elapsed.as_secs_f64() * 1000.0);
        }
    }
    samples.sort_by(f64::total_cmp);
    let mean = samples.iter().sum::<f64>() / samples.len() as f64;
    let p95 = samples[samples.len() * 95 / 100];
    let max = samples[samples.len() - 1];
    eprintln!(
        "New Bark raw 3D: mean={mean:.3}ms p95={p95:.3}ms max={max:.3}ms frames={}",
        samples.len()
    );
    assert!(max < 16.714, "3D exceeded native frame budget: {max:.3}ms");
}

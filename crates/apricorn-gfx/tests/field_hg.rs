//! ROM-gated field (3D) layer goldens — the bedroom through the full
//! pipeline (field → BG0 → compositor), pinned by SHA-1 only. Set
//! `APRICORN_RENDER_OUT` to an ignored directory to write review PNGs
//! (`field-bedroom-{gender}.png`); no ROM pixels are committed.
//!
//! The camera is preset 4 at the player's tile (`FieldScene::bedroom`:
//! map 64, tile (6, 6)) and the player texture is the one billboard, as
//! `apricorn_gfx::field::scene_view` builds it.

use apricorn_core::{
    assets::AssetStore,
    field::FieldScene,
    frame::{FieldFrame, LogicalFrame},
};
use apricorn_gfx::field::{self, Camera, CameraPreset};
use sha1::{Digest, Sha1};
use std::{path::Path, sync::Arc};

const ROM_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../hg_usa.nds");

/// The engine-A screen hashes for the two player genders — the whole
/// pipeline's pin: NSBMD decode, camera, rasterizer, billboard,
/// compositor (BG0 at priority 1 over the backdrop).
///
/// Re-pinned when `FieldFrame::static_scene` took the live system's
/// billboard anchor (`SPRITE_OFFSET`, 6.5 units toward the camera):
/// the player's hair box moved from rows 72-78 to 77-83, which is the
/// live bedroom frame's and the oracle frame 4816's (124,77,130,83)
/// exactly; nothing else in the frame moved.
const BEDROOM_GOLDEN: [&str; 2] = [
    "5299d14f0c683d9b6e6cd7eff3387d746a120f42",
    "2b9e8211ef53e2ec5cd7689f32cbc754b2d302d1",
];

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

fn write_png(dir: &str, name: &str, pixels: &[[u8; 4]]) {
    std::fs::create_dir_all(dir).unwrap();
    let file = std::fs::File::create(Path::new(dir).join(name)).unwrap();
    let mut enc = png::Encoder::new(file, 256, 192);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header()
        .unwrap()
        .write_image_data(pixels.as_flattened())
        .unwrap();
}

#[test]
fn bedroom_golden_through_bg0() {
    let Some(store) = open_rom() else {
        return;
    };
    for gender in 0..2u8 {
        let scene = FieldScene::bedroom(&store, gender).unwrap();
        let mut frame = LogicalFrame::default();
        frame.main.field = Some(FieldFrame::static_scene(Arc::new(scene)));
        frame.main.bgs[0].enabled = true;
        frame.main.bgs[0].priority = 1;
        let [main, _] = apricorn_gfx::render(&frame, &store);
        let pixels = main.as_rgba();
        if let Ok(dir) = std::env::var("APRICORN_RENDER_OUT") {
            write_png(&dir, &format!("field-bedroom-{gender}.png"), pixels);
        }
        let hash = sha1(pixels);
        assert_eq!(
            hash,
            BEDROOM_GOLDEN[usize::from(gender)],
            "bedroom gender {gender} engine A hash"
        );
    }
}

#[test]
fn bedroom_field_plane_is_covered_and_the_player_stands_at_the_centre() {
    let Some(store) = open_rom() else {
        return;
    };
    let scene = FieldScene::bedroom(&store, 0).unwrap();
    assert_eq!(
        (scene.player.width, scene.player.height),
        (32, 32),
        "the player NSBTX frame is the mmdl_m32x32 quad's size"
    );
    let position = scene.position;
    let view = FieldFrame::static_scene(Arc::new(scene));
    let mut out = vec![[0u8; 4]; 256 * 192];
    let mut depth = vec![0.0; 256 * 192];
    field::render(&view, &mut out, &mut depth);
    // The room fills most of the plane (≈28.7k of 49k pixels); the
    // rest — above the back wall, beside the side walls — is clear.
    let covered = out.iter().filter(|p| p[3] != 0).count();
    assert!(covered > 25_000, "covered {covered}");
    // The player's tile centre projects to (128, 96): the 32×32
    // billboard rises from there, so a body texel a few rows above
    // the anchor sits at the biased anchor depth — nearer than the
    // floor two rows below the feet (which is itself ≈2.6 units
    // nearer than the anchor: the bias of ≈5.2 units covers it).
    let feet = 96 * 256 + 128;
    let body = 85 * 256 + 128;
    assert!(
        depth[body] < depth[feet + 256 * 2],
        "the sprite beats the floor"
    );
    let scene_view = field::scene_view(&view);
    let centre = scene_view
        .camera
        .project_fixed(field::tile_position(position))
        .unwrap();
    assert_eq!(&centre[..2], &[128, 96]);
    // With BG0 disabled the compositor shows the backdrop instead.
    let mut frame = LogicalFrame::default();
    frame.main.field = Some(view);
    frame.main.backdrop = 0x7C00; // pure blue in BGR555
    let [main, _] = apricorn_gfx::render(&frame, &store);
    assert_eq!(main.pixel(128, 96), [0, 0, 255, 255]);
}

#[test]
fn outdoor_preset_projects_the_bedroom_too() {
    // The general camera path on real geometry: the perspective preset
    // sees the same room, differently framed — nothing panics, the
    // plane is covered, and the projection scale differs from ortho.
    let Some(store) = open_rom() else {
        return;
    };
    let scene = FieldScene::bedroom(&store, 1).unwrap();
    let target = field::tile_position(scene.position);
    let frame_view = FieldFrame::static_scene(Arc::new(scene));
    let mut view = field::scene_view(&frame_view);
    view.camera = Camera::from_preset(&CameraPreset::OUTDOOR, target);
    let mut out = vec![[0u8; 4]; 256 * 192];
    let mut depth = vec![0.0; 256 * 192];
    field::render_view(&view, &mut out, &mut depth);
    let covered = out.iter().filter(|p| p[3] != 0).count();
    assert!(covered > 30_000, "covered {covered}");
    if let Ok(dir) = std::env::var("APRICORN_RENDER_OUT") {
        write_png(&dir, "field-bedroom-outdoor.png", &out);
    }
}

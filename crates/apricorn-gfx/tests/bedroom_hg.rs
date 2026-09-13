//! ROM-gated field landing regression and optional review images.
use apricorn_core::{
    assets::AssetStore,
    field::FieldScene,
    frame::{FieldFrame, LogicalFrame},
};
use std::{path::Path, sync::Arc};

/// The field's BG0 as the game shows it: the plane is turned on once
/// the map is up (`src/field/fieldmap.c:749`,
/// `GfGfx_EngineATogglePlanes(GX_PLANEMASK_BG0, 1)`) at priority 1 —
/// below the text layers (BG3 is priority 0 in `sBgTemplate_3`) and
/// above the priority-3 BG1/BG2 (`gf_3d_render.c:48` is the simple
/// manager's `G2_SetBG0Priority(1)` reference).
fn field_frame(scene: FieldScene) -> LogicalFrame {
    let mut frame = LogicalFrame::default();
    frame.main.field = Some(FieldFrame::static_scene(Arc::new(scene)));
    frame.main.bgs[0].enabled = true;
    frame.main.bgs[0].priority = 1;
    frame
}

#[test]
fn bedroom_renders_real_models_and_both_players() {
    let path = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../hg_usa.nds"));
    if !path.exists() {
        eprintln!("skipping: no ROM at {}", path.display());
        return;
    }
    let store = AssetStore::open(path).unwrap();
    let mut images = Vec::new();
    for gender in 0..2 {
        let scene = FieldScene::bedroom(&store, gender).unwrap();
        assert_eq!(scene.map_id, 64);
        assert_eq!(scene.position, [6, 6]);
        assert!(scene.meshes.len() > 8);
        assert!(
            scene
                .meshes
                .iter()
                .map(|m| m.primitives.len())
                .sum::<usize>()
                > 250
        );
        let frame = field_frame(scene);
        let screens = apricorn_gfx::render(&frame, &store);
        let pixels = screens[0].as_rgba();
        assert!(pixels.iter().filter(|c| c[..3] != [0, 0, 0]).count() > 15000);
        if let Ok(dir) = std::env::var("APRICORN_RENDER_OUT") {
            std::fs::create_dir_all(&dir).unwrap();
            let file = std::fs::File::create(Path::new(&dir).join(format!("bedroom-{gender}.png")))
                .unwrap();
            let mut enc = png::Encoder::new(file, 256, 192);
            enc.set_color(png::ColorType::Rgba);
            enc.set_depth(png::BitDepth::Eight);
            enc.write_header()
                .unwrap()
                .write_image_data(pixels.as_flattened())
                .unwrap();
        }
        images.push(pixels.to_vec());
    }
    assert_ne!(
        images[0], images[1],
        "gender selects the actual overworld texture"
    );
}

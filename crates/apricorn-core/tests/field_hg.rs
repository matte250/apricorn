//! Integration tests: the field data layer against a retail HeartGold
//! (US) dump — map headers, matrices, land data, area data, events,
//! script headers, the ov01 tables and `FieldScene`.
//!
//! Skips silently when `hg_usa.nds` is absent (each developer supplies
//! their own ROM). Expected values are pret's `src/data/map_headers.h`
//! records with their constants resolved, structural counts measured on
//! the retail archives, and the two ov01 rows named in `docs/field-data.md`.

use std::collections::BTreeMap;
use std::path::Path;

use apricorn_core::assets::AssetStore;
use apricorn_core::field::{
    CellWindow, FieldScene, LOAD_RADIUS,
    area::{self, AreaData},
    events::{self, MapEvents},
    land::{self, LandData},
    map_header::{self, FollowMode, MapHeader, MapHeaders, MapType},
    matrix::{self, MapMatrix},
    model::{FX32_ONE, Primitive},
    ov01::{self, CameraPreset, CameraPresets, Ov01, SpriteModelTable},
    script_header::{self, InitScripts},
    terrain::TILE_BEHAVIOR_NONE,
};

const ROM_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../hg_usa.nds");

/// `MAP_NEW_BARK_PLAYER_HOUSE_2F` (`include/constants/maps.h:68`).
const BEDROOM: u16 = 64;
/// `MAP_NEW_BARK` (`include/constants/maps.h:64`), on the overworld matrix.
const NEW_BARK: u16 = 60;
/// New Bark Town's cell on matrix 0 (its records' `worldMapX/Y`).
const NEW_BARK_CELL: (i32, i32) = (21, 12);
/// `MAPSEC_CLIFF_EDGE_GATE` (`include/constants/map_sections.h:238`), the
/// last map section.
const MAX_MAPSEC: u8 = 234;

fn open() -> Option<AssetStore> {
    let path = Path::new(ROM_PATH);
    if !path.exists() {
        eprintln!("skipping: {ROM_PATH} not found (supply your own ROM dump)");
        return None;
    }
    Some(AssetStore::open(path).expect("the pinned retail dump opens"))
}

/// Map 64 as `src/data/map_headers.h` spells it.
fn bedroom_header() -> MapHeader {
    MapHeader {
        wild_encounter_bank: map_header::NO_WILD_ENCOUNTERS,
        area_data_bank: 25,
        move_model_bank: 15,
        world_map_x: 21,
        world_map_y: 12,
        matrix_id: 72,
        scripts_bank: 846,
        script_header_bank: 619,
        msg_bank: 546,
        day_music_id: 1018,
        night_music_id: 1018,
        events_bank: 61,
        mapsec: 126,
        area_icon: 9,
        mom_call_intro_param: 0,
        region_no: 0,
        weather: 0,
        map_type: MapType::Interior,
        camera_type: 4,
        follow_mode: FollowMode::HeightRestrict,
        battle_bg: 6,
        bike_allowed: false,
        running_allowed_unused: true,
        escape_rope_allowed: false,
        fly_allowed: false,
        outgoing_calls: true,
        incoming_calls: true,
        radio_signal: true,
    }
}

#[test]
fn map_headers_match_pret_and_the_pinned_address() {
    let Some(store) = open() else { return };
    let headers = MapHeaders::load(&store).unwrap();
    assert_eq!(headers.len(), map_header::MAP_HEADER_COUNT);
    let bedroom = *headers.get(BEDROOM).unwrap();
    assert_eq!(bedroom, bedroom_header());
    assert_eq!(headers.name(BEDROOM), Some("T20R0202"));
    assert_eq!(headers.name(NEW_BARK), Some("T20"));
    assert_eq!(headers.name(0), Some("EVERYWHERE"));

    // Neighbours decode to pret's records (`MAP_NEW_BARK_PLAYER_HOUSE_1F`,
    // `MAP_NEW_BARK_SOUTHWEST_HOUSE`): same flags, own banks.
    let h63 = headers.get(63).unwrap();
    assert_eq!(
        (
            h63.matrix_id,
            h63.scripts_bank,
            h63.script_header_bank,
            h63.msg_bank,
            h63.events_bank
        ),
        (71, 845, 618, 545, 60)
    );
    let h65 = headers.get(65).unwrap();
    assert_eq!(
        (
            h65.matrix_id,
            h65.scripts_bank,
            h65.script_header_bank,
            h65.msg_bank,
            h65.events_bank
        ),
        (66, 847, 620, 547, 62)
    );
    for h in [h63, h65] {
        assert_eq!(
            MapHeader {
                matrix_id: bedroom.matrix_id,
                scripts_bank: bedroom.scripts_bank,
                script_header_bank: bedroom.script_header_bank,
                msg_bank: bedroom.msg_bank,
                events_bank: bedroom.events_bank,
                ..*h
            },
            bedroom
        );
    }
    // `MAP_NEW_BARK`: outdoor, on matrix 0, camera 0, follow allowed.
    let nb = headers.get(NEW_BARK).unwrap();
    assert_eq!((nb.matrix_id, nb.area_data_bank, nb.camera_type), (0, 2, 0));
    assert_eq!(nb.map_type, MapType::CityTown);
    assert_eq!(nb.follow_mode, FollowMode::Allow);
    assert_eq!(
        (
            nb.scripts_bank,
            nb.script_header_bank,
            nb.msg_bank,
            nb.events_bank
        ),
        (842, 615, 542, 57)
    );
    assert!(nb.has_wild_encounters() && !bedroom.has_wild_encounters());
    assert!(nb.bike_allowed && nb.fly_allowed && !nb.escape_rope_allowed);

    // Every record's banks index real members.
    let matrices = store.member_count(matrix::MATRIX_NARC).unwrap();
    let scripts = store.member_count(script_header::SCRIPT_NARC).unwrap();
    let events = store.member_count(events::EVENTS_NARC).unwrap();
    let areas = store.member_count(area::AREA_NARC).unwrap();
    let msgs = store.member_count("a/0/2/7").unwrap();
    for (id, h) in headers.iter().enumerate() {
        assert!(usize::from(h.matrix_id) < matrices, "map {id} matrix");
        assert!(usize::from(h.scripts_bank) < scripts, "map {id} scripts");
        assert!(
            usize::from(h.script_header_bank) < scripts,
            "map {id} script header"
        );
        assert!(usize::from(h.events_bank) < events, "map {id} events");
        assert!(usize::from(h.area_data_bank) < areas, "map {id} area");
        assert!(usize::from(h.msg_bank) < msgs, "map {id} msg");
        assert!(
            usize::from(h.camera_type) < ov01::CAMERA_PRESET_COUNT,
            "map {id} camera"
        );
        assert!(h.mapsec <= MAX_MAPSEC, "map {id} mapsec");
        assert_eq!(
            MapHeader::parse(&h.encode()).unwrap(),
            *h,
            "map {id} roundtrip"
        );
    }
    assert_eq!(map_header::record_address(BEDROOM), 0x020F_71E0);
}

#[test]
fn matrices_parse_and_the_bedroom_is_one_cell() {
    let Some(store) = open() else { return };
    assert_eq!(
        store.member_count(matrix::MATRIX_NARC).unwrap(),
        matrix::MATRIX_COUNT
    );
    let m = MapMatrix::load(&store, 72, BEDROOM).unwrap();
    assert_eq!((m.width, m.height), (1, 1));
    assert!(!m.has_headers && !m.has_altitudes);
    assert_eq!(m.name, "m_hh0102_");
    assert_eq!(m.headers, vec![BEDROOM]);
    assert_eq!(m.land_ids, vec![217]);
    assert_eq!(m.cell_origin(0, 0), Some([256 << 12, 0, 256 << 12]));

    let world = MapMatrix::load(&store, 0, NEW_BARK).unwrap();
    assert_eq!((world.width, world.height), (47, 17));
    assert_eq!(world.cell_count(), matrix::MAX_CELLS);
    assert!(world.has_headers && world.has_altitudes);
    assert_eq!(
        world.header(NEW_BARK_CELL.0, NEW_BARK_CELL.1),
        Some(NEW_BARK)
    );
    assert_eq!(world.land_id(NEW_BARK_CELL.0, NEW_BARK_CELL.1), Some(0));
    assert_eq!(world.altitude(NEW_BARK_CELL.0, NEW_BARK_CELL.1), Some(0));
    assert_eq!(world.land_id(0, 0), Some(matrix::NO_LAND));

    let mut cells = 0;
    let mut largest = 0;
    for id in 0..matrix::MATRIX_COUNT as u16 {
        let m = MapMatrix::load(&store, id, 0).unwrap();
        assert!(m.cell_count() <= matrix::MAX_CELLS);
        assert!(m.name.len() <= matrix::MAX_NAME_LENGTH);
        cells += m.cell_count();
        largest = largest.max(m.cell_count());
    }
    assert_eq!(largest, matrix::MAX_CELLS);
    assert!(cells > 1000);
}

#[test]
fn land_data_sections_and_attributes() {
    let Some(store) = open() else { return };
    assert_eq!(
        store.member_count(land::LAND_NARC).unwrap(),
        land::LAND_COUNT
    );
    let bedroom = LandData::load(&store, 217).unwrap();
    let s = bedroom.sizes;
    assert_eq!(
        (s.attributes, s.props, s.model, s.bdhc, s.extra),
        (2048, 384, 6576, 144, 0)
    );
    assert_eq!(bedroom.props.len(), 8);
    assert!(bedroom.extra.is_empty());
    assert_eq!(&bedroom.model[..4], b"BMD0");
    let mut histogram = BTreeMap::new();
    for a in bedroom.attributes {
        *histogram.entry(a).or_insert(0usize) += 1;
    }
    assert_eq!(
        histogram,
        BTreeMap::from([(0x0000, 936), (0x005F, 1), (0x8000, 83), (0x8086, 4)])
    );
    for p in &bedroom.props {
        assert_eq!(p.rotation, [0; 3]);
        assert_eq!(p.scale, [FX32_ONE; 3]);
    }
    assert_eq!(bedroom.bdhc.counts, [6, 2, 2, 3, 2, 4]);

    // Members with an extra section: it sits between the header and the
    // attributes (the BMD0 magic lands at 0x14 + extra + attr + props).
    // New Bark Town's member 0 is the evidence for the order: read from
    // 0x14 + extraSize, its four door tiles (0x8069) are the map's four
    // warp events — (684,393), (695,396), (679,405), (690,407) in map
    // 60's matrix, cell (21, 12) — and its last 44 words are the tree
    // run the old order mistook for the extra section.
    let nb = LandData::load(&store, 0).unwrap();
    assert_eq!((nb.sizes.props, nb.sizes.extra), (816, 88));
    assert_eq!(nb.props.len(), 17);
    assert_eq!(nb.props[0].model_id, 21);
    assert_eq!(
        nb.props[0].translation,
        [-24 * FX32_ONE, 16 * FX32_ONE, -128 * FX32_ONE]
    );
    let doors: Vec<(usize, usize)> = (0..32)
        .flat_map(|z| (0..32).map(move |x| (x, z)))
        .filter(|&(x, z)| nb.attribute(x, z) == Some(0x8069))
        .map(|(x, z)| (672 + x, 384 + z))
        .collect();
    assert_eq!(doors, [(684, 393), (695, 396), (679, 405), (690, 407)]);
    assert!(nb.attributes[1024 - 44..].iter().all(|&w| w == 0x8006));
    assert_eq!(nb.extra.len(), 88);

    let mut props = 0;
    let mut with_extra = 0;
    let mut rotated = 0;
    let mut scaled = 0;
    let mut max_props = 0;
    for id in 0..land::LAND_COUNT as u16 {
        let l = LandData::load(&store, id).unwrap();
        assert!(l.props.len() <= land::MAX_PROPS, "land {id}");
        props += l.props.len();
        max_props = max_props.max(l.props.len());
        with_extra += usize::from(!l.extra.is_empty());
        rotated += l.props.iter().filter(|p| p.rotation != [0; 3]).count();
        scaled += l.props.iter().filter(|p| p.scale != [FX32_ONE; 3]).count();
        assert!(l.sizes.bdhc > 0, "land {id} BDHC");
    }
    assert_eq!((props, max_props, with_extra), (2859, 30, 230));
    assert_eq!(
        (rotated, scaled),
        (0, 0),
        "retail never rotates or scales a prop"
    );
}

#[test]
fn area_data_selects_the_archives() {
    let Some(store) = open() else { return };
    assert_eq!(
        store.member_count(area::AREA_NARC).unwrap(),
        area::AREA_COUNT
    );
    let a25 = AreaData::load(&store, 25).unwrap();
    assert_eq!(
        (
            a25.building_set,
            a25.map_texture,
            a25.unknown,
            a25.outdoor,
            a25.light_selector
        ),
        (1, 25, 0xFFFF, 0, 0)
    );
    assert_eq!(a25.prop_model_narc(), area::BM_ROOM_NARC);
    let ids = a25.building_ids(&store).unwrap();
    assert_eq!(ids.len(), 58);
    assert_eq!(&ids[..4], &[1, 2, 4, 89]);
    let a2 = AreaData::load(&store, 2).unwrap();
    assert!(a2.is_outdoor());
    assert_eq!(a2.prop_model_narc(), area::BM_FIELD_NARC);
    assert_eq!(a2.prop_animation_narc(), area::BM_FIELD_ANIM_NARC);
    for bank in 0..area::AREA_COUNT as u8 {
        let a = AreaData::load(&store, bank).unwrap();
        assert!(usize::from(a.building_set) < store.member_count(area::BUILDING_SET_NARC).unwrap());
        assert!(usize::from(a.map_texture) < store.member_count(area::MAP_TEXTURE_NARC).unwrap());
        a.building_ids(&store).unwrap();
    }
}

#[test]
fn events_and_script_headers() {
    let Some(store) = open() else { return };
    assert_eq!(
        store.member_count(events::EVENTS_NARC).unwrap(),
        events::EVENTS_COUNT
    );
    let e = MapEvents::load(&store, 61).unwrap();
    assert_eq!(
        (e.bg.len(), e.objects.len(), e.warps.len(), e.coords.len()),
        (2, 3, 1, 0)
    );
    assert_eq!(e.warps[0].x, 3);
    assert_eq!(e.warps[0].z, 4);
    assert_eq!(e.warps[0].header, 63, "the stairs lead to the house's 1F");
    assert_eq!(e.warps[0].anchor, 1);
    assert_eq!(e.warp_at(3, 4), Some(0));
    assert_eq!(e.objects[0].sprite_id, 376);
    assert_eq!((e.objects[0].x, e.objects[0].z), (8, 10));
    for bank in 0..events::EVENTS_COUNT as u16 {
        MapEvents::load(&store, bank).unwrap();
    }

    // The bedroom's header is empty; its neighbours carry typed records.
    let s619 = InitScripts::load(&store, 619).unwrap();
    assert!(s619.records.is_empty() && s619.frame_table.is_empty());
    let s615 = InitScripts::load(&store, 615).unwrap(); // New Bark Town
    let kinds: Vec<u8> = s615.records.iter().map(|r| r.kind).collect();
    assert_eq!(
        kinds,
        vec![
            script_header::ON_FRAME_TABLE,
            script_header::ON_TRANSITION,
            script_header::ON_RESUME
        ]
    );
    assert_eq!(s615.on_transition(), Some(7));
    assert_eq!(s615.on_resume(), Some(10));
    assert_eq!(s615.on_load(), None);
    assert_eq!(s615.frame_table.len(), 2);
    assert_eq!(
        (s615.frame_table[0].var_a, s615.frame_table[0].var_b),
        (0x4106, 1)
    );
    assert_eq!(s615.frame_table[0].script_id, 4);
    let s618 = InitScripts::load(&store, 618).unwrap(); // house 1F
    assert_eq!(s618.records.len(), 1);
    assert_eq!(s618.frame_table.len(), 2);
    assert_eq!(s618.frame_table[1].script_id, 1);
}

#[test]
fn ov01_tables_read_from_the_overlay() {
    let Some(store) = open() else { return };
    let ov = Ov01::load(&store).unwrap();
    assert_eq!(ov.ram_address, ov01::LOAD_ADDRESS);
    assert_eq!(ov.len(), 0x24260);
    let cams = CameraPresets::from_overlay(&ov).unwrap();
    assert_eq!(cams.presets().len(), ov01::CAMERA_PRESET_COUNT);
    assert_eq!(
        *cams.get(4).unwrap(),
        CameraPreset {
            distance: 0x0061_B89B,
            angle: [0xDC82, 0, 0],
            padding: 0,
            perspective_type: 1,
            fovy_angle: 0x0281,
            near: 0x0009_6000,
            far: 0x006C_7000,
            look_at_offset: [0; 3],
        }
    );
    assert_eq!(
        *cams.get(0).unwrap(),
        CameraPreset {
            distance: 0x0029_AEC1,
            angle: [0xDD62, 0, 0],
            padding: 0,
            perspective_type: 0,
            fovy_angle: 0x05C1,
            near: 0x0009_6000,
            far: 0x004B_0000,
            look_at_offset: [0; 3],
        }
    );
    assert!(cams.get(17).is_none());
    // The land model's base transform (`ov01_02206BD8` / `ov01_02206BE4`)
    // is unit scale and identity rotation — read, not assumed.
    let base = ov.land_base().unwrap();
    assert_eq!(base, ov01::LandBase::IDENTITY);
    assert_eq!(base.at([1, 2, 3]).translation, [1, 2, 3]);
    for p in cams.presets() {
        assert_eq!(p.padding, 0);
        assert!(p.perspective_type <= 1);
        assert!(p.near > 0 && p.far > p.near);
    }
    let sprites = SpriteModelTable::from_overlay(&ov).unwrap();
    assert_eq!(sprites.model_for_sprite(ov01::SPRITE_HERO), Some(69));
    assert_eq!(sprites.model_for_sprite(ov01::SPRITE_HEROINE), Some(70));
    assert_eq!(sprites.entry(ov01::SPRITE_HERO).unwrap().packed, 0x1C60);
    assert_eq!(sprites.entries().len(), 901);
    let mmodels = store.member_count(ov01::MMODEL_NARC).unwrap();
    assert!(
        sprites
            .entries()
            .iter()
            .all(|e| usize::from(e.mmodel_id) < mmodels)
    );
}

#[test]
fn bedroom_scene_keeps_the_phase_4_shape() {
    let Some(store) = open() else { return };
    for gender in 0..2u8 {
        let scene = FieldScene::bedroom(&store, gender).unwrap();
        assert_eq!(
            FieldScene::load(&store, BEDROOM, 6, 6, gender).unwrap(),
            scene
        );
        assert_eq!(scene.map_id, BEDROOM);
        assert_eq!(scene.name, "T20R0202");
        assert_eq!(scene.header, bedroom_header());
        assert_eq!(scene.position, [6, 6]);
        assert_eq!(
            scene.window,
            CellWindow {
                x0: 0,
                z0: 0,
                width: 1,
                height: 1
            }
        );
        assert_eq!(scene.cells.len(), 1);
        assert_eq!(scene.cells[0].land_id, 217);
        assert_eq!(scene.cells[0].origin, [256 << 12, 0, 256 << 12]);
        assert_eq!(scene.props.len(), 8);
        assert!(
            scene
                .props
                .iter()
                .all(|p| p.model_id == p.placement.model_id)
        );
        let cell_meshes = scene.cells[0].meshes.len();
        let prop_meshes: usize = scene.props.iter().map(|p| p.meshes.len()).sum();
        assert_eq!(scene.meshes.len(), cell_meshes + prop_meshes);
        assert!(scene.meshes.len() > 8);
        assert!(
            scene
                .meshes
                .iter()
                .map(|m| m.primitives.len())
                .sum::<usize>()
                > 250
        );
        assert_eq!(scene.camera.perspective_type, 1);
        assert_eq!(scene.camera.angle[0], 0xDC82);
        assert_eq!((scene.player.width, scene.player.height), (32, 32));
        assert_eq!(scene.terrain.behavior(6, 6), 0);
        assert!(!scene.terrain.impassable(6, 6));
        assert!(scene.terrain.impassable(0, 0), "the wall row");
        assert_eq!(scene.terrain.behavior(40, 40), TILE_BEHAVIOR_NONE);
        assert_eq!(scene.terrain.loaded_cells(), 1);
        assert_eq!(scene.events.warps.len(), 1);
        assert!(scene.init_scripts.records.is_empty());
        assert!(scene.cell_at(6, 6).is_some() && scene.cell_at(32, 0).is_none());
        // Every world vertex lies inside the cell (0..512 units, y small).
        for v in scene
            .meshes
            .iter()
            .flat_map(|m| m.primitives.iter())
            .flat_map(|primitive| primitive.vertices())
        {
            assert!((0..=512 << 12).contains(&v.position[0]));
            assert!((0..=512 << 12).contains(&v.position[2]));
            assert!(v.position[1].abs() < 128 << 12);
        }
    }
    let male = FieldScene::bedroom(&store, 0).unwrap();
    let female = FieldScene::bedroom(&store, 1).unwrap();
    assert_ne!(male.player, female.player);
    assert_eq!(male.meshes, female.meshes);
}

#[test]
fn new_bark_town_loads_a_window_of_the_overworld() {
    let Some(store) = open() else { return };
    let (x, z) = (NEW_BARK_CELL.0 * 32 + 16, NEW_BARK_CELL.1 * 32 + 16);
    let scene = FieldScene::load(&store, NEW_BARK, x, z, 0).unwrap();
    assert_eq!(scene.name, "T20");
    assert_eq!(scene.matrix.id, 0);
    assert!(scene.area.is_outdoor());
    assert_eq!(
        scene.window,
        CellWindow {
            x0: 20,
            z0: 11,
            width: 3,
            height: 3
        }
    );
    assert_eq!(LOAD_RADIUS, 1);
    assert_eq!(
        scene.cells.len(),
        9,
        "every cell around New Bark has land data"
    );
    let home = scene.cell_at(x, z).unwrap();
    assert_eq!((home.cell_x, home.cell_z), (21, 12));
    assert_eq!(home.map_id, NEW_BARK);
    assert_eq!(home.land_id, 0);
    assert_eq!(
        home.origin,
        [(21 * 512 + 256) << 12, 0, (12 * 512 + 256) << 12]
    );
    // The ocean cells to the north sit two altitude steps up.
    let north = scene.cell_at(x, z - 32).unwrap();
    assert_eq!(north.origin[1], 2 << 15);
    assert_eq!(scene.terrain.loaded_cells(), 9);
    assert_eq!(scene.terrain.behavior(x - 64, z), TILE_BEHAVIOR_NONE);
    assert_ne!(scene.terrain.attr(x, z), None);
    // Props: bm_field models placed at the cell's x/z, never its altitude.
    let home_props: Vec<_> = scene.props.iter().filter(|p| p.cell == [21, 12]).collect();
    assert_eq!(home_props.len(), 17);
    assert!(scene.props.len() > 17);
    for p in &scene.props {
        assert_eq!(
            p.model_id, p.placement.model_id,
            "every placement is in the building set"
        );
        let cell = scene
            .cells
            .iter()
            .find(|c| [c.cell_x, c.cell_z] == p.cell)
            .unwrap();
        assert_eq!(
            p.translation[0],
            cell.origin[0] + p.placement.translation[0]
        );
        assert_eq!(p.translation[1], p.placement.translation[1]);
        assert_eq!(
            p.translation[2],
            cell.origin[2] + p.placement.translation[2]
        );
        assert!(
            !p.meshes.is_empty(),
            "prop model {} has geometry",
            p.model_id
        );
    }
    // Model 27 (a rotated-node bm_field prop) is in the window and its
    // node transform moved geometry off the cell's model origin.
    assert!(scene.props.iter().any(|p| p.model_id == 27));
    assert_eq!(scene.camera.perspective_type, 0);
    // `files/fielddata/eventdata/zone_event/057_T20.json`: 5 signs, 10
    // objects, 5 warps (the doors) and 4 coord triggers, all on New Bark's
    // cell; the player's front door leads to `MAP_NEW_BARK_PLAYER_HOUSE_1F`.
    let e = &scene.events;
    assert_eq!(
        (e.bg.len(), e.objects.len(), e.warps.len(), e.coords.len()),
        (5, 10, 5, 4)
    );
    assert_eq!(
        (
            e.warps[1].x,
            e.warps[1].z,
            e.warps[1].header,
            e.warps[1].anchor
        ),
        (695, 396, 63, 0)
    );
    for w in &e.warps {
        let cell = scene.cell_at(i32::from(w.x), i32::from(w.z)).unwrap();
        assert_eq!(cell.map_id, NEW_BARK);
    }
    assert_eq!(scene.init_scripts.on_transition(), Some(7));
    assert_eq!(scene.position, [x, z]);
    assert!(
        scene
            .meshes
            .iter()
            .flat_map(|mesh| &mesh.primitives)
            .map(|primitive| match primitive {
                Primitive::Triangle(_) => 1,
                Primitive::Quad(_) => 2,
            })
            .sum::<usize>()
            > 5000
    );
}

#[test]
fn every_bm_field_model_parses_with_node_transforms() {
    let Some(store) = open() else { return };
    // A sub-matrix loaded whole: Sprout Tower's 2x3 (`m_dun0101_`, matrix 2)
    // through any map that uses it; and the 3x3 matrix 27.
    let headers = MapHeaders::load(&store).unwrap();
    for (matrix_id, cells) in [(2u16, 6usize), (27, 9)] {
        let (map_id, _) = headers
            .iter()
            .enumerate()
            .find(|(_, h)| h.matrix_id == matrix_id)
            .map(|(id, h)| (id as u16, *h))
            .expect("a map uses the matrix");
        let scene = FieldScene::load_all(&store, map_id, 8, 8, 1).unwrap();
        assert_eq!(scene.window, CellWindow::all(&scene.matrix));
        assert_eq!(scene.cells.len(), cells);
        assert_eq!(scene.terrain.loaded_cells(), cells);
        assert_eq!(scene.matrix.id, matrix_id);
    }
}

//! Rasterizer tests — synthetic fixtures, no ROM.
//!
//! Every fixture is a hand-built cache chunk parsed through the same
//! `cache::parse` readers the asset store produces (and, for the
//! window tests, a minimal font through `Font::parse`), wrapped in a
//! [`FixtureStore`] that implements [`AssetSource`]. The assertions
//! are hand-computed pixels (flips, banks, blend weights, brightness,
//! window fill/glyph/scroll/frame/arrow/focus) plus one golden-hash test
//! pinning a composite scene by its SHA-1 — hashes only, never
//! pixels, per the house rule.

use apricorn_core::cache;
use apricorn_core::font::Font;
use apricorn_core::frame::{
    ARROW_TILE_OFFSETS, AssetId, BgLayer, Blend, BlendEffect, BrightnessMode, ColorMode,
    EngineFrame, LogicalFrame, MasterBrightness, PaletteLoad, Sprite, TextColor, TilePlacement,
    Window, WindowArrow, WindowFocus, WindowFrame, WindowGlyph, plane,
};
use apricorn_gfx::{AssetSource, ScreenBuffer, render};
use sha1::{Digest, Sha1};

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// One fixture asset.
enum Fix {
    Tiles(cache::Tiles),
    Screen(cache::Screen),
    Palette(Vec<[u8; 4]>),
    Font(Font),
    Cells(cache::Cells),
    Animation(cache::Animation),
}

/// An in-memory [`AssetSource`]: handles index the added assets in
/// load order, exactly like the real store.
struct FixtureStore {
    items: Vec<Fix>,
}

impl FixtureStore {
    fn add_sprite(&mut self, tiles: AssetId, palette: AssetId) -> Sprite {
        self.add_sprite_with_bank(tiles, palette, 0)
    }

    fn add_sprite_with_bank(&mut self, tiles: AssetId, palette: AssetId, bank: u8) -> Sprite {
        let header = |kind: cache::ChunkKind| {
            let mut chunk = cache::MAGIC.to_vec();
            chunk.extend_from_slice(&cache::VERSION.to_le_bytes());
            chunk.extend_from_slice(&kind.code().to_le_bytes());
            chunk
        };
        let mut chunk = header(cache::ChunkKind::Cells);
        chunk.extend_from_slice(&[1, 0, 0, 0]); // one cell, 1D32K
        chunk.extend_from_slice(&[1, 0, 0, 0]); // one OAM entry
        for v in [-2i16, 252, 0, 252, 510, 0] {
            chunk.extend_from_slice(&v.to_le_bytes());
        }
        chunk.extend_from_slice(&[bank, 0, 0, 0, 0, 0]); // palette, priority, shape, size, flags, mode
        chunk.extend_from_slice(&[0; 2]); // no labels
        let cells = AssetId::from_index(self.items.len());
        self.items
            .push(Fix::Cells(cache::Cells::parse(&chunk).unwrap()));
        let mut chunk = header(cache::ChunkKind::Animation);
        chunk.extend_from_slice(&[1, 0]); // one sequence
        chunk.extend_from_slice(&[1, 0, 0, 0, 1, 0, 0, 0]); // one frame, forward, cell-only
        chunk.extend_from_slice(&[0, 0, 1, 0]); // cell 0, duration 1
        chunk.extend_from_slice(&[0; 16]); // translation, rotation, scale
        let animation = AssetId::from_index(self.items.len());
        self.items
            .push(Fix::Animation(cache::Animation::parse(&chunk).unwrap()));
        Sprite {
            tiles,
            palette,
            cells,
            animation,
            sequence: 0,
            elapsed: 0,
            x: 4,
            y: 6,
            priority: 0,
            palette_bank: 1,
        }
    }
    fn new() -> Self {
        Self { items: Vec::new() }
    }

    /// Parses and stores a Tiles chunk.
    fn add_tiles(&mut self, fmt: u8, pixels: &[u8]) -> AssetId {
        assert!(pixels.len() % 64 == 0, "whole 8×8 tiles");
        let mut chunk = Vec::new();
        chunk.extend_from_slice(&cache::MAGIC);
        chunk.extend_from_slice(&cache::VERSION.to_le_bytes());
        chunk.extend_from_slice(&cache::ChunkKind::Tiles.code().to_le_bytes());
        chunk.push(fmt); // 0 = 4bpp (expanded), 1 = 8bpp
        chunk.push(0); // mapping: TwoD
        chunk.push(0); // flags
        chunk.push(0); // pad
        chunk.extend_from_slice(&((pixels.len() / 64) as u32).to_le_bytes());
        chunk.extend_from_slice(&(pixels.len() as u32).to_le_bytes());
        chunk.extend_from_slice(pixels);
        self.items.push(Fix::Tiles(
            cache::Tiles::parse(&chunk).expect("fixture tiles parse"),
        ));
        AssetId::from_index(self.items.len() - 1)
    }

    /// Parses and stores a Screen chunk — one u16 entry per 8×8 cell.
    fn add_screen(&mut self, width: u16, height: u16, entries: &[u16]) -> AssetId {
        assert_eq!(
            entries.len(),
            usize::from(width / 8) * usize::from(height / 8),
            "entries cover the screen"
        );
        let mut chunk = Vec::new();
        chunk.extend_from_slice(&cache::MAGIC);
        chunk.extend_from_slice(&cache::VERSION.to_le_bytes());
        chunk.extend_from_slice(&cache::ChunkKind::Screen.code().to_le_bytes());
        chunk.extend_from_slice(&width.to_le_bytes());
        chunk.extend_from_slice(&height.to_le_bytes());
        chunk.extend_from_slice(&0u16.to_le_bytes()); // color mode
        chunk.extend_from_slice(&0u16.to_le_bytes()); // text format
        chunk.extend_from_slice(&((entries.len() * 2) as u32).to_le_bytes());
        for entry in entries {
            chunk.extend_from_slice(&entry.to_le_bytes());
        }
        self.items.push(Fix::Screen(
            cache::Screen::parse(&chunk).expect("fixture screen parses"),
        ));
        AssetId::from_index(self.items.len() - 1)
    }

    /// Parses and stores a Palette chunk (no PMCP).
    fn add_palette(&mut self, bpp4: bool, colors: &[[u8; 4]]) -> AssetId {
        let mut chunk = Vec::new();
        chunk.extend_from_slice(&cache::MAGIC);
        chunk.extend_from_slice(&cache::VERSION.to_le_bytes());
        chunk.extend_from_slice(&cache::ChunkKind::Palette.code().to_le_bytes());
        chunk.push(u8::from(bpp4)); // flags: bit0 = 16-color
        chunk.push(0); // pad
        chunk.extend_from_slice(&0u16.to_le_bytes()); // pad
        chunk.extend_from_slice(&(colors.len() as u32).to_le_bytes());
        chunk.extend_from_slice(&0u16.to_le_bytes()); // no PMCP
        chunk.extend_from_slice(&0u16.to_le_bytes()); // pad
        for color in colors {
            chunk.extend_from_slice(color);
        }
        let parsed = cache::Palette::parse(&chunk).expect("fixture palette parses");
        self.items.push(Fix::Palette(parsed.rgba().to_vec()));
        AssetId::from_index(self.items.len() - 1)
    }

    /// Parses and stores a minimal 8×8 font: `glyphs` are the first
    /// glyph rows (each glyph one uniform level per row), padded to
    /// the 428-glyph fallback coverage the parser demands.
    fn add_font(&mut self, glyphs: &[&[u8; 8]]) -> AssetId {
        const NUM_GLYPHS: usize = 428;
        let mut data = Vec::new();
        data.extend_from_slice(&((16 + NUM_GLYPHS) as u32).to_le_bytes()); // headerSize
        data.extend_from_slice(&16u32.to_le_bytes()); // widthDataStart
        data.extend_from_slice(&(NUM_GLYPHS as u32).to_le_bytes());
        data.push(0); // fixedWidth
        data.push(8); // fixedHeight
        data.push(1); // glyphWidth: 1 tile
        data.push(1); // glyphHeight: 1 tile
        for _ in 0..NUM_GLYPHS {
            data.push(8); // every glyph advances 8
        }
        for index in 0..NUM_GLYPHS {
            // Glyph `index`: the given rows (level L per row: both
            // half rows the byte L·0b01010101... pairs), or zero.
            for row in 0..8 {
                let level = glyphs.get(index).map_or(0, |rows| rows[row]);
                let byte = level * 0b01010101;
                data.push(byte); // low byte: pixels 4-7
                data.push(byte); // high byte: pixels 0-3
            }
        }
        self.items
            .push(Fix::Font(Font::parse(&data).expect("fixture font parses")));
        AssetId::from_index(self.items.len() - 1)
    }
}

impl AssetSource for FixtureStore {
    fn cells(&self, id: AssetId) -> Option<&cache::Cells> {
        match self.items.get(id.index()) {
            Some(Fix::Cells(cells)) => Some(cells),
            _ => None,
        }
    }
    fn animation(&self, id: AssetId) -> Option<&cache::Animation> {
        match self.items.get(id.index()) {
            Some(Fix::Animation(animation)) => Some(animation),
            _ => None,
        }
    }
    fn tiles(&self, id: AssetId) -> Option<&cache::Tiles> {
        match self.items.get(id.index()) {
            Some(Fix::Tiles(tiles)) => Some(tiles),
            _ => None,
        }
    }

    fn screen(&self, id: AssetId) -> Option<&cache::Screen> {
        match self.items.get(id.index()) {
            Some(Fix::Screen(screen)) => Some(screen),
            _ => None,
        }
    }

    fn placed_palette(&self, id: AssetId) -> Option<&[[u8; 4]]> {
        match self.items.get(id.index()) {
            Some(Fix::Palette(colors)) => Some(colors),
            _ => None,
        }
    }

    fn font(&self, id: AssetId) -> Option<&Font> {
        match self.items.get(id.index()) {
            Some(Fix::Font(font)) => Some(font),
            _ => None,
        }
    }
}

/// A text screen entry: tile, flips, palette bank.
fn entry(tile: u16, h_flip: bool, v_flip: bool, bank: u16) -> u16 {
    tile | u16::from(h_flip) << 10 | u16::from(v_flip) << 11 | bank << 12
}

/// A palette of `count` colors where color `i` is `[i, i, i, 255]` —
/// every lookup result is directly readable.
fn gray_palette(count: usize) -> Vec<[u8; 4]> {
    (0..count)
        .map(|i| [i as u8, i as u8, i as u8, 255])
        .collect()
}

/// An enabled 4bpp BG layer.
fn bg_layer(screen: AssetId) -> BgLayer {
    BgLayer {
        enabled: true,
        screen: Some(screen),
        ..BgLayer::default()
    }
}

/// A whole-file palette load at slot offset 0 — the fixture idiom for
/// `GXLoadPal(…, GF_PAL_SLOT_0_OFFSET, …)`.
fn load_palette(asset: AssetId) -> PaletteLoad {
    PaletteLoad {
        asset,
        offset: 0,
        // The fixture palettes are all ≤ 256 colors; the RAM clip
        // bounds the load either way.
        colors: 256,
    }
}

/// One tile-set placement at tile offset 0.
fn place(tiles: AssetId) -> TilePlacement {
    TilePlacement {
        asset: tiles,
        tile: 0,
    }
}

/// One engine: `layers` at BG0.., tiles placed per `(slot, asset)`,
/// and one whole-file palette load.
fn engine(layers: &[BgLayer], slots: &[(usize, AssetId)], palette: AssetId) -> EngineFrame {
    let mut engine = EngineFrame::default();
    for (i, layer) in layers.iter().enumerate() {
        engine.bgs[i] = *layer;
    }
    for &(slot, tiles) in slots {
        engine.char_blocks[slot].push(place(tiles));
    }
    engine.palette_loads.push(load_palette(palette));
    engine
}

// ---------------------------------------------------------------------------
// Screen entries and tile fetch
// ---------------------------------------------------------------------------

#[test]
fn flips_decode_from_the_entry_bits() {
    let mut store = FixtureStore::new();
    // One 4bpp tile: pixel value at (x, y) is x + y + 1 (1..=15).
    let pixels: Vec<u8> = (0..64).map(|i| (i % 8 + i / 8 + 1) as u8).collect();
    let tiles = store.add_tiles(0, &pixels);
    // Four cells: plain, h-flip, v-flip, both.
    let entries = [
        entry(0, false, false, 0),
        entry(0, true, false, 0),
        entry(0, false, true, 0),
        entry(0, true, true, 0),
    ];
    let screen = store.add_screen(32, 8, &entries);
    let palette = store.add_palette(true, &gray_palette(16));
    let frame = LogicalFrame {
        main: engine(&[bg_layer(screen)], &[(0, tiles)], palette),
        ..LogicalFrame::default()
    };

    let [main, _] = render(&frame, &store);
    let v = |x, y| main.pixel(x, y)[0]; // gray palette: r == value
    assert_eq!(v(3, 2), 3 + 2 + 1, "plain reads (x, y)");
    // x=9..15 is cell 1: the h-flipped tile.
    assert_eq!(v(9, 2), (7 - 1) + 2 + 1, "h-flip mirrors x within 0..8");
    assert_eq!(v(17, 3), 1 + (7 - 3) + 1, "v-flip mirrors y within 0..8");
    assert_eq!(v(25, 4), (7 - 1) + (7 - 4) + 1, "hv mirrors both");
}

#[test]
fn bpp4_picks_the_entry_bank_and_bpp8_reads_the_whole_palette() {
    let mut store = FixtureStore::new();
    // One tile of value 3 everywhere.
    let tiles = store.add_tiles(0, &[3u8; 64]);
    // Entries: bank 0 (cell 0), bank 1 (cell 1), bank 2 with 8bpp.
    let entries = [entry(0, false, false, 0), entry(0, false, false, 1)];
    let screen = store.add_screen(16, 8, &entries);
    let palette = store.add_palette(true, &gray_palette(48));
    let frame = LogicalFrame {
        main: engine(&[bg_layer(screen)], &[(0, tiles)], palette),
        ..LogicalFrame::default()
    };

    let [main, _] = render(&frame, &store);
    assert_eq!(main.pixel(0, 0)[0], 3, "bank 0: colors 0..15");
    assert_eq!(main.pixel(8, 0)[0], 16 + 3, "bank 1: colors 16..31");

    // 8bpp: the bank bits are ignored, value 200 reads color 200.
    let mut store = FixtureStore::new();
    let tiles = store.add_tiles(1, &[200u8; 64]);
    let screen = store.add_screen(8, 8, &[entry(0, false, false, 15)]);
    let palette = store.add_palette(false, &gray_palette(256));
    let mut layer = bg_layer(screen);
    layer.color_mode = ColorMode::Bpp8;
    let frame = LogicalFrame {
        main: engine(&[layer], &[(0, tiles)], palette),
        ..LogicalFrame::default()
    };
    let [main, _] = render(&frame, &store);
    assert_eq!(main.pixel(4, 4)[0], 200, "8bpp: full palette, bank ignored");
}

#[test]
fn color_zero_is_transparent_and_shows_the_layer_below() {
    let mut store = FixtureStore::new();
    // Top tile: value 0 (transparent); bottom tile: value 1.
    let top = store.add_tiles(0, &[0u8; 64]);
    let bottom = store.add_tiles(0, &[1u8; 64]);
    let screen_top = store.add_screen(16, 8, &[entry(0, false, false, 0), 0]);
    let screen_bottom = store.add_screen(16, 8, &[entry(0, false, false, 0); 2]);
    let palette = store.add_palette(true, &gray_palette(16));
    let mut frame = LogicalFrame::default();
    let mut e = EngineFrame::default();
    e.bgs[0] = BgLayer {
        enabled: true,
        screen: Some(screen_top),
        priority: 0,
        ..BgLayer::default()
    };
    e.bgs[1] = BgLayer {
        enabled: true,
        screen: Some(screen_bottom),
        priority: 0,
        ..BgLayer::default()
    };
    e.char_blocks[0].push(place(bottom));
    e.char_blocks[1].push(place(top));
    e.bgs[0].char_base = 1;
    e.palette_loads.push(load_palette(palette));
    frame.main = e;

    let [main, _] = render(&frame, &store);
    assert_eq!(
        main.pixel(0, 0)[0],
        1,
        "hole over cell 0 shows BG1's tile 1"
    );
    assert_eq!(
        main.pixel(8, 0)[0],
        1,
        "cell 1: top transparent, bottom tile 1"
    );
}

#[test]
fn later_placements_win_an_overlap_and_palette_loads_compose_in_order() {
    let mut store = FixtureStore::new();
    // Tile 0 red in one set, green in another; both placed at tile 0
    // of slot 0 — the later placement is the VRAM overwrite.
    let red = store.add_tiles(0, &[1u8; 64]);
    let green = store.add_tiles(0, &[2u8; 64]);
    let screen = store.add_screen(8, 8, &[entry(0, false, false, 0)]);
    let palette = store.add_palette(true, &[[0, 0, 0, 255], [255, 0, 0, 255], [0, 255, 0, 255]]);
    let mut e = engine(&[bg_layer(screen)], &[], palette);
    e.char_blocks[0].push(place(red));
    e.char_blocks[0].push(place(green));
    let frame = LogicalFrame {
        main: e,
        ..LogicalFrame::default()
    };
    let [main, _] = render(&frame, &store);
    assert_eq!(
        main.pixel(0, 0),
        [0, 255, 0, 255],
        "the later placement wins"
    );

    // Palette RAM composes in order: a 1-color load at offset 2
    // overwrites the green the winning tile displays.
    let over = store.add_palette(true, &[[9, 9, 9, 255]]);
    let mut frame = frame;
    frame.main.palette_loads.push(PaletteLoad {
        asset: over,
        offset: 2,
        colors: 1,
    });
    let [main, _] = render(&frame, &store);
    assert_eq!(
        main.pixel(0, 0),
        [9, 9, 9, 255],
        "the later load overwrites"
    );

    // A load past RAM's end clips, not panics.
    frame.main.palette_loads.push(PaletteLoad {
        asset: over,
        offset: 255,
        colors: 100,
    });
    let [main, _] = render(&frame, &store);
    assert_eq!(
        main.pixel(0, 0),
        [9, 9, 9, 255],
        "the clip keeps RAM intact"
    );
}

// ---------------------------------------------------------------------------
// Scroll and map geometry
// ---------------------------------------------------------------------------

#[test]
fn scroll_wraps_within_the_layer_map() {
    let mut store = FixtureStore::new();
    // Tile n is uniform color n+1; the map's entry (tx, ty) = tile tx,
    // so the color at output x names the map column.
    let pixels: Vec<u8> = (0..4).flat_map(|n| [n + 1; 64]).collect();
    let tiles = store.add_tiles(0, &pixels);
    let entries: Vec<u16> = (0..32 * 4)
        .map(|i| entry(u16::try_from(i % 32).unwrap(), false, false, 0))
        .collect();
    let screen = store.add_screen(256, 32, &entries);
    let palette = store.add_palette(true, &gray_palette(16));
    let mut frame = LogicalFrame::default();
    let mut layer = bg_layer(screen);
    layer.scroll_x = 8; // one tile
    frame.main = engine(&[layer], &[(0, tiles)], palette);

    let [main, _] = render(&frame, &store);
    assert_eq!(
        main.pixel(0, 0)[0],
        2,
        "scroll 8: output 0 reads map column 8"
    );
    assert_eq!(main.pixel(8, 0)[0], 3, "output 8 reads map column 16");

    // Raw scroll is 9-bit: 264 wraps to 8 inside the 256-wide map.
    let mut layer = bg_layer(screen);
    layer.scroll_x = 264;
    frame.main = engine(&[layer], &[(0, tiles)], palette);
    let [main, _] = render(&frame, &store);
    assert_eq!(main.pixel(0, 0)[0], 2, "264 % 256 = 8: same column");
}

#[test]
fn maps_shorter_than_the_layer_read_entry_zero() {
    let mut store = FixtureStore::new();
    // Tile 0 is color 1; tile 1 is color 2. A 256×192 chunk (24 tile
    // rows) on a 256×256 layer: rows 192..255 read entry 0.
    let pixels: Vec<u8> = [1u8; 64].iter().copied().chain([2u8; 64]).collect();
    let tiles = store.add_tiles(0, &pixels);
    let entries: Vec<u16> = (0..32 * 24).map(|_| entry(1, false, false, 0)).collect();
    let screen = store.add_screen(256, 192, &entries);
    let palette = store.add_palette(true, &gray_palette(16));
    let mut layer = bg_layer(screen);
    layer.scroll_y = 64; // rows 192..255 of the map become rows 128..191
    let frame = LogicalFrame {
        main: engine(&[layer], &[(0, tiles)], palette),
        ..LogicalFrame::default()
    };

    let [main, _] = render(&frame, &store);
    assert_eq!(
        main.pixel(0, 127)[0],
        2,
        "the chunk's last row shows tile 1"
    );
    assert_eq!(
        main.pixel(0, 128)[0],
        1,
        "beyond the chunk: entry 0 → tile 0"
    );
}

// ---------------------------------------------------------------------------
// Priority and blend
// ---------------------------------------------------------------------------

/// Two full-cover BG layers of uniform tiles: BG0 shows `colors[0]`,
/// BG1 shows `colors[1]`, priorities per `priorities`.
fn cover_engine(store: &mut FixtureStore, priorities: [u8; 2], colors: [u8; 2]) -> LogicalFrame {
    let palette = store.add_palette(true, &gray_palette(16));
    let mut e = EngineFrame::default();
    for i in 0..2 {
        let tiles = store.add_tiles(0, &[colors[i]; 64]);
        let screen = store.add_screen(8, 8, &[entry(0, false, false, 0)]);
        e.bgs[i] = BgLayer {
            enabled: true,
            screen: Some(screen),
            priority: priorities[i],
            char_base: i as u8,
            ..BgLayer::default()
        };
        e.char_blocks[i].push(place(tiles));
    }
    e.palette_loads.push(load_palette(palette));
    LogicalFrame {
        main: e,
        ..LogicalFrame::default()
    }
}

#[test]
fn priority_orders_layers_and_ties_go_to_the_lower_bg() {
    // BG0 color 1, BG1 color 2.
    let mut store = FixtureStore::new();
    let frame = cover_engine(&mut store, [0, 0], [1, 2]);
    let [main, _] = render(&frame, &store);
    assert_eq!(main.pixel(0, 0)[0], 1, "tie: BG0 wins");

    let mut store = FixtureStore::new();
    let frame = cover_engine(&mut store, [1, 0], [1, 2]);
    let [main, _] = render(&frame, &store);
    assert_eq!(main.pixel(0, 0)[0], 2, "BG1 has the lower priority");

    // A disabled higher layer is out of the stack entirely.
    let mut store = FixtureStore::new();
    let mut frame = cover_engine(&mut store, [0, 1], [1, 2]);
    frame.main.bgs[0].enabled = false;
    let [main, _] = render(&frame, &store);
    assert_eq!(main.pixel(0, 0)[0], 2, "BG0 off: BG1 shows");
}

#[test]
fn alpha_blend_mixes_the_first_and_second_targets() {
    let mut store = FixtureStore::new();
    // BG0 red (plane1), BG1 blue (plane2), EVA = EBV = 8.
    let red = store.add_tiles(0, &[1u8; 64]);
    let blue = store.add_tiles(0, &[2u8; 64]);
    let s0 = store.add_screen(8, 8, &[entry(0, false, false, 0)]);
    let s1 = store.add_screen(8, 8, &[entry(0, false, false, 0)]);
    let palette = store.add_palette(true, &[[0, 0, 0, 255], [255, 0, 0, 255], [0, 0, 255, 255]]);
    let mut e = EngineFrame::default();
    e.bgs[0] = BgLayer {
        enabled: true,
        screen: Some(s0),
        ..BgLayer::default()
    };
    e.bgs[1] = BgLayer {
        enabled: true,
        screen: Some(s1),
        ..BgLayer::default()
    };
    e.char_blocks[0].push(place(red));
    e.char_blocks[1].push(place(blue));
    e.bgs[0].char_base = 0;
    e.bgs[1].char_base = 1;
    e.palette_loads.push(load_palette(palette));
    e.blend = Blend {
        plane1: plane::BG0,
        effect: BlendEffect::Alpha,
        plane2: plane::BG1,
        eva: 8,
        ebv: 8,
        evy: 0,
    };
    let frame = LogicalFrame {
        main: e,
        ..LogicalFrame::default()
    };

    let [main, _] = render(&frame, &store);
    // (255·8 + 0·8 + 8) >> 4 = 128 for r; b symmetrically; g stays 0.
    assert_eq!(main.pixel(0, 0), [128, 0, 128, 255]);

    // Weights: EVA 31 clamps to 16 (the register's 0-16 range) —
    // the first target alone, exactly.
    let mut frame = frame;
    frame.main.blend.eva = 31;
    frame.main.blend.ebv = 0;
    let [main, _] = render(&frame, &store);
    assert_eq!(main.pixel(0, 0), [255, 0, 0, 255], "EBV 0: pure first");
    // EVA 0, EBV 31 clamps to 16 → the second target alone.
    frame.main.blend.eva = 0;
    frame.main.blend.ebv = 31;
    let [main, _] = render(&frame, &store);
    assert_eq!(main.pixel(0, 0), [0, 0, 255, 255], "EVA 0: pure second");
}

#[test]
fn blending_rules_for_untargeted_and_unmatched_pixels() {
    let mut store = FixtureStore::new();
    // BG0 (top) green, BG1 red, backdrop white; only BG1 blends.
    let green = store.add_tiles(0, &[1u8; 64]);
    let red = store.add_tiles(0, &[2u8; 64]);
    let s0 = store.add_screen(8, 8, &[entry(0, false, false, 0)]);
    let s1 = store.add_screen(8, 8, &[entry(0, false, false, 0)]);
    let palette = store.add_palette(true, &[[0, 0, 0, 255], [0, 255, 0, 255], [255, 0, 0, 255]]);
    let mut e = EngineFrame::default();
    e.bgs[0] = BgLayer {
        enabled: true,
        screen: Some(s0),
        priority: 0,
        ..BgLayer::default()
    };
    e.bgs[1] = BgLayer {
        enabled: true,
        screen: Some(s1),
        priority: 1,
        ..BgLayer::default()
    };
    e.char_blocks[0].push(place(green));
    e.char_blocks[1].push(place(red));
    e.bgs[0].char_base = 0;
    e.bgs[1].char_base = 1;
    e.palette_loads.push(load_palette(palette));
    // plane1 = BG1, plane2 = BD (the white backdrop).
    e.blend = Blend {
        plane1: plane::BG1,
        effect: BlendEffect::Alpha,
        plane2: plane::BD,
        eva: 8,
        ebv: 8,
        evy: 0,
    };
    e.backdrop = 0x7FFF; // BGR555 white
    let frame = LogicalFrame {
        main: e,
        ..LogicalFrame::default()
    };

    // The displayed pixel is BG0's green — not in plane1 — so it
    // passes through unblended even though BG1 below is a target.
    let [main, _] = render(&frame, &store);
    assert_eq!(
        main.pixel(0, 0),
        [0, 255, 0, 255],
        "top outside plane1: unblended"
    );

    // Disable BG0: BG1 is displayed and blends against the backdrop.
    let mut frame = frame;
    frame.main.bgs[0].enabled = false;
    let [main, _] = render(&frame, &store);
    // r: (255·8 + 255·8 + 8) >> 4 = 255; g/b: (0 + 255·8 + 8) >> 4 = 128.
    assert_eq!(main.pixel(0, 0), [255, 128, 128, 255], "BG1 over white BD");

    // plane2 empty: no second target anywhere — the first target
    // shows unblended.
    frame.main.bgs[0].enabled = true;
    frame.main.bgs[0].priority = 1;
    frame.main.bgs[1].priority = 0;
    frame.main.blend.plane2 = 0;
    let [main, _] = render(&frame, &store);
    assert_eq!(
        main.pixel(0, 0),
        [255, 0, 0, 255],
        "no second target: unblended"
    );
}

// ---------------------------------------------------------------------------
// Windows
// ---------------------------------------------------------------------------

/// A 16×16 window on BG0 at (2, 2), palette bank 2, fill 0xF, one
/// glyph at its top-left — the fill/glyph/scroll fixture.
fn window_frame(store: &mut FixtureStore) -> (LogicalFrame, AssetId) {
    // Glyph 1's row y is level 1 + y % 3 (fg, shadow, bg of the
    // print's color triple); glyph 2 is uniformly level 3.
    let rows1 = [1u8, 2, 3, 1, 2, 3, 1, 2];
    let rows2 = [3u8; 8];
    let font = store.add_font(&[&rows1, &rows2]);
    let palette = store.add_palette(true, &gray_palette(64));
    let mut e = EngineFrame::default();
    e.bgs[0] = BgLayer {
        enabled: true,
        ..BgLayer::default()
    };
    e.palette_loads.push(load_palette(palette));
    e.windows.push(Window {
        bg: 0,
        left: 2,
        top: 2,
        width: 2,
        height: 2,
        palette: 2,
        base_tile: 0,
        fill: 0xF,
        fills: Vec::new(),
        glyphs: vec![
            WindowGlyph {
                font,
                glyph: 1,
                x: 0,
                y: 0,
                color: TextColor::new(1, 2, 3),
                double_rows: false,
            },
            WindowGlyph {
                font,
                glyph: 2,
                x: 0,
                y: 8,
                color: TextColor::new(1, 2, 3),
                double_rows: false,
            },
        ],
        scroll: 0,
        frame: None,
        arrow: None,
        focus: None,
    });
    (
        LogicalFrame {
            main: e,
            ..LogicalFrame::default()
        },
        font,
    )
}

#[test]
fn window_fill_and_glyphs_composite_into_the_owning_layer() {
    let mut store = FixtureStore::new();
    let (frame, _font) = window_frame(&mut store);
    let [main, _] = render(&frame, &store);

    // The window rect is (16, 16)-(32, 32); the palette bank is 2, so
    // the fill 0xF is color 2·16+15 = 47 and the glyph's levels map
    // fg 1 → 33, shadow 2 → 34, bg 3 → 35.
    let v = |x, y| main.pixel(x, y)[0];
    assert_eq!(v(15, 15), 0, "outside the rect: the backdrop");
    assert_eq!(v(16, 16), 33, "glyph row 0: level 1 → fg");
    assert_eq!(v(16, 17), 34, "glyph row 1: level 2 → shadow");
    assert_eq!(v(16, 18), 35, "glyph row 2: level 3 → bg");
    assert_eq!(v(16, 23), 34, "glyph row 7: level 2");
    assert_eq!(v(16, 24), 35, "the second glyph's row 0: level 3 → bg");
    assert_eq!(v(24, 16), 47, "past the glyph's width: the fill");
    assert_eq!(v(16, 31), 35, "the second glyph's last row: level 3");
    assert_eq!(v(31, 31), 47, "the rect's far corner: the fill");

    // The window rides its layer: disabled, it is gone.
    let mut frame = frame;
    frame.main.bgs[0].enabled = false;
    let [main, _] = render(&frame, &store);
    assert_eq!(main.pixel(16, 16)[0], 0, "the layer off: the window hides");

    // The window beats the screen map: an entry under the rect never
    // shows while the window covers it.
    let mut frame = frame;
    frame.main.bgs[0].enabled = true;
    let screen = store.add_screen(8, 8, &[entry(0, false, false, 0)]);
    let tiles = store.add_tiles(0, &[5u8; 64]);
    frame.main.bgs[0].screen = Some(screen);
    frame.main.char_blocks[0].push(place(tiles));
    let [main, _] = render(&frame, &store);
    assert_eq!(main.pixel(0, 0)[0], 5, "outside the rect: the map");
    assert_eq!(main.pixel(16, 16)[0], 33, "inside: the window's glyph");
}

#[test]
fn window_scroll_shifts_content_up_within_the_rect() {
    let mut store = FixtureStore::new();
    let (frame, _font) = window_frame(&mut store);

    // Scroll 8: content at unscrolled row r shows at row r - 8 — the
    // second glyph (uniform level 3 → color 35) moves to the top.
    let mut frame = frame;
    frame.main.windows[0].scroll = 8;
    let [main, _] = render(&frame, &store);
    let v = |x, y| main.pixel(x, y)[0];
    assert_eq!(v(16, 16), 35, "the second glyph scrolled to the top");
    assert_eq!(v(16, 24), 47, "below it: the fill (the first glyph left)");
}

#[test]
fn window_frame_and_arrow_draw_from_the_owning_char_block() {
    let mut store = FixtureStore::new();
    // 30 uniform tiles, tile n = value (n % 15) + 1: the frame border
    // (tiles 0-8) and the arrow's first two frames (18-21, 22-25).
    let pixels: Vec<u8> = (0..30).flat_map(|n| [((n % 15) + 1) as u8; 64]).collect();
    let tiles = store.add_tiles(0, &pixels);
    let palette = store.add_palette(true, &gray_palette(64));
    let mut e = EngineFrame::default();
    e.bgs[0] = BgLayer {
        enabled: true,
        ..BgLayer::default()
    };
    e.char_blocks[0].push(place(tiles));
    e.palette_loads.push(load_palette(palette));
    // A 2×2 window at (1, 1): frame tiles ring it, the arrow sits at
    // tiles (4..5, 1..2).
    e.windows.push(Window {
        bg: 0,
        left: 1,
        top: 1,
        width: 2,
        height: 2,
        palette: 2,
        base_tile: 0,
        fill: 1,
        glyphs: Vec::new(),
        fills: Vec::new(),
        scroll: 0,
        frame: Some(WindowFrame {
            base_tile: 0,
            palette: 3,
            dialogue: false,
        }),
        arrow: Some(WindowArrow {
            base_tile: 0,
            index: 0,
        }),
        focus: None,
    });
    let frame = LogicalFrame {
        main: e,
        ..LogicalFrame::default()
    };

    let [main, _] = render(&frame, &store);
    let v = |x, y| main.pixel(x, y)[0];
    // Frame palette bank 3: tile n's color is 48 + (n % 15) + 1.
    assert_eq!(v(0, 0), 48 + 1, "TL corner: border tile 0");
    assert_eq!(v(8, 0), 48 + 2, "top edge: border tile 1");
    assert_eq!(v(24, 0), 48 + 3, "TR corner: border tile 2");
    assert_eq!(v(0, 8), 48 + 4, "left edge: border tile 3");
    assert_eq!(v(24, 8), 48 + 6, "right edge: border tile 5");
    assert_eq!(v(0, 24), 48 + 7, "BL corner: border tile 6");
    assert_eq!(v(8, 24), 48 + 8, "bottom edge: border tile 7");
    assert_eq!(v(24, 24), 48 + 9, "BR corner: border tile 8");
    assert_eq!(v(8, 8), 32 + 1, "the interior: the fill at bank 2");

    // The arrow at tiles (4..5, 1..2), palette bank 1: frame 0 draws
    // tiles 18 + pos (offset {0,1,2,1}[0] = 0).
    assert_eq!(v(32, 8), 16 + ((18 % 15) + 1), "arrow pos 0: tile 18");
    assert_eq!(v(40, 8), 16 + ((19 % 15) + 1), "arrow pos 1: tile 19");
    assert_eq!(v(32, 16), 16 + ((20 % 15) + 1), "arrow pos 2: tile 20");
    assert_eq!(v(40, 16), 16 + ((21 % 15) + 1), "arrow pos 3: tile 21");

    // Animation index 2: offset ARROW_TILE_OFFSETS[2] = 2 → tiles
    // 26 + pos.
    let mut frame = frame;
    frame.main.windows[0].arrow = Some(WindowArrow {
        base_tile: 0,
        index: 2,
    });
    let [main, _] = render(&frame, &store);
    assert_eq!(
        main.pixel(32, 8)[0],
        16 + ((26 % 15) + 1),
        "index 2: tile 26"
    );
    assert_eq!(
        ARROW_TILE_OFFSETS,
        [0, 1, 2, 1],
        "the cycle the index walks"
    );
}

#[test]
fn window_focus_indicator_blits_its_frame_in_the_reserved_column() {
    let mut store = FixtureStore::new();
    // The focus NCGR: 48 tiles — four 12-tile frames of three per
    // row — tile i uniform value i % 16 (tiles 0, 16, 32 are color 0,
    // the blit's colorKey).
    let pixels: Vec<u8> = (0..48).flat_map(|i| [(i % 16) as u8; 64]).collect();
    let focus_gfx = store.add_tiles(0, &pixels);
    // A glyph under the column the indicator reserves: its levels
    // show wherever the indicator's own pixels are color 0.
    let rows1 = [1u8, 2, 3, 1, 2, 3, 1, 2];
    let font = store.add_font(&[&rows1]);
    let palette = store.add_palette(true, &gray_palette(64));
    let mut e = EngineFrame::default();
    e.bgs[0] = BgLayer {
        enabled: true,
        ..BgLayer::default()
    };
    e.palette_loads.push(load_palette(palette));
    // A 6×4-tile window at (1, 1): destX = (6-3)·8 = 24
    // window-relative, the blit's 24×32 covering the interior's
    // right half.
    e.windows.push(Window {
        bg: 0,
        left: 1,
        top: 1,
        width: 6,
        height: 4,
        palette: 2,
        base_tile: 0,
        fill: 0xF,
        fills: Vec::new(),
        glyphs: vec![WindowGlyph {
            font,
            glyph: 1,
            x: 24,
            y: 0,
            color: TextColor::new(7, 8, 9),
            double_rows: false,
        }],
        scroll: 0,
        frame: None,
        arrow: None,
        focus: Some(WindowFocus {
            asset: focus_gfx,
            index: 0,
            scroll: 0,
        }),
    });
    let frame = LogicalFrame {
        main: e,
        ..LogicalFrame::default()
    };
    let [main, _] = render(&frame, &store);
    let v = |x, y| main.pixel(x, y)[0];
    // The window origin is (8, 8); its bank-2 palette makes focus
    // value i color 32+i, the fill 0xF color 47, the glyph's fg 7
    // color 39.
    assert_eq!(v(32, 8), 39, "tile 0 is the key: the glyph beneath shows");
    assert_eq!(v(40, 8), 33, "(8, 0) is tile 1 — three per row, tile-major");
    assert_eq!(v(32, 16), 35, "(0, 8) is tile 3 — the second row of three");
    assert_eq!(v(55, 39), 43, "(23, 31) is tile 11 — the frame's last");
    assert_eq!(v(31, 8), 47, "left of the column: the fill");

    // Frame 1: the same pixels read tiles 12..23.
    let mut frame = frame;
    frame.main.windows[0].focus = Some(WindowFocus {
        asset: focus_gfx,
        index: 1,
        scroll: 0,
    });
    let [main, _] = render(&frame, &store);
    assert_eq!(main.pixel(40, 8)[0], 45, "frame 1's (8, 0) is tile 13");

    // A scroll landing after the blit lifts it with the content —
    // its last rows vacate to the fill — while the print-time scroll
    // rides along unchanged.
    frame.main.windows.last_mut().unwrap().scroll = 4;
    let [main, _] = render(&frame, &store);
    assert_eq!(
        main.pixel(40, 36)[0],
        47,
        "the blit lifted 4 rows: fy 32 falls out"
    );
    frame.main.windows[0].focus = Some(WindowFocus {
        asset: focus_gfx,
        index: 1,
        scroll: 4,
    });
    let [main, _] = render(&frame, &store);
    assert_eq!(
        main.pixel(40, 36)[0],
        38,
        "focus.scroll rides the content: fy 28 → tile 22, value 6"
    );
}

#[test]
fn later_windows_draw_over_earlier_ones() {
    let mut store = FixtureStore::new();
    let palette = store.add_palette(true, &gray_palette(64));
    let mut e = EngineFrame::default();
    e.bgs[0] = BgLayer {
        enabled: true,
        ..BgLayer::default()
    };
    e.palette_loads.push(load_palette(palette));
    // Two windows covering the same rect: the later one wins.
    for (bank, fill) in [(2u8, 1u8), (3, 2)] {
        e.windows.push(Window {
            bg: 0,
            left: 0,
            top: 0,
            width: 2,
            height: 2,
            palette: bank,
            base_tile: 0,
            fill,
            glyphs: Vec::new(),
            fills: Vec::new(),
            scroll: 0,
            frame: None,
            arrow: None,
            focus: None,
        });
    }
    let frame = LogicalFrame {
        main: e,
        ..LogicalFrame::default()
    };
    let [main, _] = render(&frame, &store);
    assert_eq!(
        main.pixel(0, 0)[0],
        3 * 16 + 2,
        "the later window's fill shows"
    );
}

// ---------------------------------------------------------------------------
// Backdrop, missing assets, brightness
// ---------------------------------------------------------------------------

#[test]
fn backdrop_covers_unrendered_pixels_with_exact_expansion() {
    let frame = LogicalFrame::default();
    let store = FixtureStore::new();
    let [main, sub] = render(&frame, &store);
    assert_eq!(main.pixel(0, 0), [0, 0, 0, 255], "black default");
    assert_eq!(sub.pixel(255, 191), [0, 0, 0, 255]);

    // GX_RGB white and the 1/31 step (1 → 0x08).
    let mut frame = LogicalFrame::default();
    frame.sub.backdrop = 0x7FFF;
    assert_eq!(render(&frame, &store)[1].pixel(0, 0), [255, 255, 255, 255]);
    frame.sub.backdrop = 0x0001;
    assert_eq!(render(&frame, &store)[1].pixel(0, 0), [8, 0, 0, 255]);

    // A layer whose assets are missing (the deferred 3D BG0) is
    // transparent, not an error: the backdrop shows through.
    let mut frame = LogicalFrame::default();
    frame.sub.backdrop = 0x001F;
    frame.sub.bgs[0] = BgLayer {
        enabled: true,
        ..BgLayer::default()
    };
    assert_eq!(render(&frame, &store)[1].pixel(0, 0), [255, 0, 0, 255]);
}

#[test]
fn master_brightness_applies_last_over_the_whole_screen() {
    let mut store = FixtureStore::new();
    let tiles = store.add_tiles(0, &[1u8; 64]);
    let screen = store.add_screen(8, 8, &[entry(0, false, false, 0)]);
    let palette = store.add_palette(true, &[[0, 0, 0, 255], [255, 0, 0, 255]]);
    let mut frame = LogicalFrame {
        main: engine(&[bg_layer(screen)], &[(0, tiles)], palette),
        ..LogicalFrame::default()
    };

    // Up 16: the limit — white.
    frame.main.brightness = MasterBrightness {
        mode: BrightnessMode::Up,
        value: 16,
    };
    assert_eq!(render(&frame, &store)[0].pixel(0, 0), [255, 255, 255, 255]);
    // Down 16: the limit — black.
    frame.main.brightness = MasterBrightness {
        mode: BrightnessMode::Down,
        value: 16,
    };
    assert_eq!(render(&frame, &store)[0].pixel(0, 0), [0, 0, 0, 255]);
    // Up 8 on red: r → 255, g/b → (255-0)*8 >> 4 = 127.
    frame.main.brightness = MasterBrightness {
        mode: BrightnessMode::Up,
        value: 8,
    };
    assert_eq!(render(&frame, &store)[0].pixel(0, 0), [255, 127, 127, 255]);
    // Down 8 on red: r → 255 - (255·8 >> 4) = 128, g/b stay 0.
    frame.main.brightness = MasterBrightness {
        mode: BrightnessMode::Down,
        value: 8,
    };
    assert_eq!(render(&frame, &store)[0].pixel(0, 0), [128, 0, 0, 255]);
    // Disabled: untouched, and the unrendered area is the backdrop
    // (black), not the layer.
    frame.main.brightness = MasterBrightness::default();
    // Past the 8-px map the layer still covers: entry 0 → tile 0 (red),
    // not the backdrop.
    assert_eq!(render(&frame, &store)[0].pixel(8, 0), [255, 0, 0, 255]);
}

// ---------------------------------------------------------------------------
// Golden hash
// ---------------------------------------------------------------------------

/// The golden fixture: a scrolled 4bpp layer with a blend over a
/// second layer and master brightness, exercising the whole path.
/// Pinned by SHA-1 only — never pixels.
#[test]
fn golden_composite_scene_hashes_stably() {
    let mut store = FixtureStore::new();
    // Tiles: 0 = red, 1 = green, 2 = blue (palette values 1, 2, 3).
    let pixels: Vec<u8> = [1u8; 64]
        .iter()
        .chain([2u8; 64].iter())
        .chain([3u8; 64].iter())
        .copied()
        .collect();
    let tiles = store.add_tiles(0, &pixels);
    // A 16×16-px map: (0,0)=tile0 bank0, (1,0)=tile1 bank1, (0,1)=tile2.
    let entries = [
        entry(0, false, false, 0),
        entry(1, true, false, 0),
        entry(2, false, true, 0),
        0,
    ];
    let screen = store.add_screen(16, 16, &entries);
    let palette = store.add_palette(
        true,
        &[
            [0, 0, 0, 255],
            [255, 0, 0, 255],
            [0, 255, 0, 255],
            [0, 0, 255, 255],
        ],
    );
    let mut e = EngineFrame::default();
    e.bgs[0] = BgLayer {
        enabled: true,
        screen: Some(screen),
        scroll_x: 4,
        scroll_y: 2,
        priority: 0,
        ..BgLayer::default()
    };
    e.char_blocks[0].push(place(tiles));
    e.palette_loads.push(load_palette(palette));
    e.blend = Blend {
        plane1: plane::BG0,
        effect: BlendEffect::Alpha,
        plane2: plane::BD,
        eva: 12,
        ebv: 4,
        evy: 0,
    };
    e.backdrop = 0x03E0; // GX_RGB green 31
    e.brightness = MasterBrightness {
        mode: BrightnessMode::Down,
        value: 4,
    };
    let frame = LogicalFrame {
        main: e,
        ..LogicalFrame::default()
    };

    let [main, sub] = render(&frame, &store);
    let hex = |screen: &ScreenBuffer| {
        let flat = screen
            .as_rgba()
            .iter()
            .flatten()
            .copied()
            .collect::<Vec<u8>>();
        let mut hasher = Sha1::new();
        hasher.update(&flat);
        format!("{:x}", hasher.finalize())
    };
    // Hand-checked before pinning, e.g. (0,0): red over the green
    // backdrop → (191, 63, 0), dimmed by 1/16 → (144, 48, 0).
    assert_eq!(main.pixel(0, 0), [144, 48, 0, 255]);
    assert_eq!(
        hex(&main),
        "d3835635d347cd6bd0f5bbc22eda17701b1ec6a6",
        "engine A golden hash"
    );
    assert_eq!(
        hex(&sub),
        "31c8daa3770fde1d76332d8aac4f53357adc1ce6",
        "engine B golden hash (empty, undimmed black)"
    );
}

#[test]
fn window_zero_indices_reveal_lower_planes_not_the_replaced_tilemap() {
    let mut store = FixtureStore::new();
    let (mut frame, _) = window_frame(&mut store);
    let screen = store.add_screen(8, 8, &[entry(0, false, false, 0)]);
    let old = store.add_tiles(0, &[5; 64]);
    let under = store.add_tiles(0, &[7; 64]);
    frame.main.bgs[0].screen = Some(screen);
    frame.main.char_blocks[0].push(place(old));
    frame.main.bgs[1] = BgLayer {
        char_base: 1,
        ..bg_layer(screen)
    };
    frame.main.char_blocks[1].push(place(under));
    frame.main.windows[0].fill = 0;
    frame.main.windows[0].glyphs[0].color.fg = 0;
    let [main, _] = render(&frame, &store);
    assert_eq!(main.pixel(0, 0)[0], 5, "old BG0 outside the window");
    assert_eq!(
        main.pixel(16, 16)[0],
        7,
        "glyph ink mapped to zero reveals BG1"
    );
    assert_eq!(
        main.pixel(31, 31)[0],
        7,
        "zero fill reveals BG1, not palette-bank color 0"
    );
    assert_eq!(main.pixel(16, 17)[0], 34, "nonzero shadow remains opaque");
}

#[test]
fn obj_cells_use_signed_offsets_separate_palettes_and_hardware_priority() {
    let mut store = FixtureStore::new();
    let tiles = store.add_tiles(0, &(0..64).map(|i| (i % 8) as u8).collect::<Vec<_>>());
    let obj_palette = store.add_palette(true, &gray_palette(32));
    let sprite = store.add_sprite(tiles, obj_palette);
    let bg_tiles = store.add_tiles(0, &[9; 64]);
    let bg_palette = store.add_palette(true, &[[99, 99, 99, 255]; 16]);
    let screen = store.add_screen(8, 8, &[0]);
    let mut frame = LogicalFrame::default();
    frame.main.sprites.push(sprite);
    frame.main.bgs[0] = bg_layer(screen);
    frame.main.char_blocks[0].push(place(bg_tiles));
    frame.main.palette_loads.push(load_palette(bg_palette));
    let [main, _] = render(&frame, &store);
    assert_eq!(
        main.pixel(3, 2)[0],
        17,
        "(4,6) + signed (-2,-4), independent OBJ palette bank 1"
    );
    assert_eq!(main.pixel(2, 2)[0], 99, "OBJ index zero reveals BG0");
    assert_eq!(main.pixel(9, 9)[0], 23, "8x8 cell ends at (9,9)");
    assert_eq!(main.pixel(10, 9)[0], 99, "outside the cell");
    frame.main.sprites[0].priority = 1;
    assert_eq!(
        render(&frame, &store)[0].pixel(3, 2)[0],
        99,
        "higher-priority BG hides OBJ"
    );
    frame.main.sprites[0].priority = 0;
    frame.main.blend = Blend {
        plane1: plane::OBJ,
        effect: BlendEffect::BrightnessUp,
        evy: 16,
        ..Blend::default()
    };
    let [main, _] = render(&frame, &store);
    assert_eq!(main.pixel(3, 2), [255; 4], "OBJ participates in the flash");
    assert_eq!(main.pixel(2, 2)[0], 99, "the flash leaves BG0 alone");
}

#[test]
fn obj_cell_palette_bank_is_added_to_resource_placement() {
    let mut store = FixtureStore::new();
    let tiles = store.add_tiles(0, &[1; 64]);
    let palette = store.add_palette(true, &gray_palette(256));
    let sprite = store.add_sprite_with_bank(tiles, palette, 2);
    let mut frame = LogicalFrame::default();
    frame.main.sprites.push(sprite);
    assert_eq!(
        render(&frame, &store)[0].pixel(3, 2)[0],
        49,
        "resource bank 1 + OAM bank 2"
    );
    frame.main.sprites[0].palette_bank = 15;
    assert_eq!(
        render(&frame, &store)[0].pixel(3, 2)[0],
        17,
        "bank addition wraps at 16"
    );
}

#[test]
fn window_rect_fills_are_clipped_ordered_and_scroll_with_content() {
    let mut store = FixtureStore::new();
    let (mut frame, _) = window_frame(&mut store);
    frame.main.windows.push(Window {
        width: 2,
        height: 2,
        fill: 1,
        palette: 0,
        fills: vec![(4, 4, 8, 8, 2), (8, 8, 20, 20, 3)],
        ..Window::default()
    });
    let [screen, _] = render(&frame, &store);
    assert_eq!(screen.pixel(0, 0)[0], 1);
    assert_eq!(screen.pixel(4, 4)[0], 2);
    assert_eq!(screen.pixel(8, 8)[0], 3);
    frame.main.windows.last_mut().unwrap().scroll = 4;
    assert_eq!(render(&frame, &store)[0].pixel(4, 0)[0], 2);
}

#[test]
fn zero_color_glyph_pixels_preserve_window_background() {
    let mut store = FixtureStore::new();
    let (mut frame, font) = window_frame(&mut store);
    frame.main.windows.clear();
    frame.main.windows.push(Window {
        width: 2,
        height: 2,
        fill: 4,
        glyphs: vec![WindowGlyph {
            font,
            glyph: 2,
            x: 0,
            y: 0,
            color: TextColor::new(14, 15, 0),
            double_rows: false,
        }],
        ..Window::default()
    });
    // Fixture glyph 2 is entirely level 3 (background), mapped to 0.
    assert_eq!(render(&frame, &store)[0].pixel(0, 0)[0], 4);
}

#[test]
fn hardware_window_masks_scrolled_bg_and_its_message_windows() {
    let mut store = FixtureStore::new();
    let (mut frame, _) = window_frame(&mut store);
    frame.main.bgs[0].hidden_rect = Some((0, 0, 255, 24));
    let [screen, _] = render(&frame, &store);
    assert_eq!(screen.pixel(16, 16), [0, 0, 0, 255]);
    assert_ne!(screen.pixel(16, 24), [0, 0, 0, 255]);
}

#[test]
fn dialogue_border_uses_all_eighteen_tiles_and_arrow_keeps_border_palette() {
    let mut store = FixtureStore::new();
    let tiles = store.add_tiles(
        0,
        &(0..30)
            .flat_map(|n| [(n % 15 + 1) as u8; 64])
            .collect::<Vec<_>>(),
    );
    let palette = store.add_palette(true, &gray_palette(80));
    let mut frame = LogicalFrame::default();
    frame.main.bgs[0].enabled = true;
    frame.main.char_blocks[0].push(place(tiles));
    frame.main.palette_loads.push(load_palette(palette));
    frame.main.windows.push(Window {
        left: 2,
        top: 2,
        width: 3,
        height: 4,
        palette: 2,
        fill: 15,
        frame: Some(WindowFrame {
            base_tile: 0,
            palette: 4,
            dialogue: true,
        }),
        ..Window::default()
    });
    let [main, _] = render(&frame, &store);
    // The original writes six columns per row: two left, repeated
    // center, and three right. Mid-row center is the window buffer.
    for (y, row) in [(8, 0), (16, 1), (48, 2)] {
        for (x, column) in [(0, 0), (8, 1), (16, 2), (40, 3), (48, 4), (56, 5)] {
            if row == 1 && column == 2 {
                continue;
            }
            assert_eq!(
                main.pixel(x, y)[0],
                64 + ((row * 6 + column) % 15 + 1) as u8
            );
        }
    }
    frame.main.windows[0].arrow = Some(WindowArrow {
        base_tile: 0,
        index: 0,
    });
    let [main, _] = render(&frame, &store);
    assert_eq!(
        main.pixel(48, 32)[0],
        68,
        "arrow tile 18 replaces the side border, preserving bank 4"
    );
}

/// The dialogue box's extents, pinned against pret's geometry after a
/// retail comparison that questioned them (`docs/game-flow.md`,
/// "Rendering fixes"):
///
/// * the interior fill spans exactly `width` tiles — `sub_0200E6B4`
///   (`render_window.s`) then writes three border columns to its
///   right (`x+width`, `+1`, `+2`: tiles `+3/+4/+5`, `+9/+10/+11`,
///   `+15/+16/+17`) and two to its left. The dark band inside the
///   retail box's right end *is* border art (`+10`/`+11`), not a fill
///   shortfall, so the fill must stop at the interior's edge;
/// * `TextPrinter_DrawDownArrow` (`render_text.c:395-449`) lands its
///   2×2 block on columns `x+width+1..+2`, rows `y+height-2..-1` —
///   over `+10`/`+11` — from tiles `base+18 + {0,1,2,1}[index]·4 +
///   pos`, keeping the border's palette bank (`0x10`); index 3 is
///   index 1's tiles;
/// * `RenderScreenFocusIndicatorTile` (`text.c:296-306`) blits the
///   `{YESNO}` screen-focus icon *inside* the buffer at
///   `(width-3)·8` — it never reaches the border or the arrow block.
#[test]
fn dialogue_fill_border_arrow_and_focus_extents_follow_pret() {
    let mut store = FixtureStore::new();
    // Frame tiles: tile n uniform value n % 15 + 1 (bank 4 → 64 + v).
    let frame_tiles = store.add_tiles(
        0,
        &(0..30)
            .flat_map(|n| [(n % 15 + 1) as u8; 64])
            .collect::<Vec<_>>(),
    );
    // The focus NCGR: four 12-tile frames, tile i uniform i % 16 + 1
    // shifted so no tile is the colorKey 0 (values 1..=15, then 1).
    let focus_gfx = store.add_tiles(
        0,
        &(0..48)
            .flat_map(|i| [(i % 15 + 1) as u8; 64])
            .collect::<Vec<_>>(),
    );
    let palette = store.add_palette(true, &gray_palette(96));
    let mut frame = LogicalFrame::default();
    frame.main.bgs[0].enabled = true;
    frame.main.char_blocks[0].push(place(frame_tiles));
    frame.main.palette_loads.push(load_palette(palette));
    // sWindowTemplate_DialogMsg's shape (27×4 at (2, 19)) scaled to
    // 5×4 at (2, 2): interior pixels (16..56, 16..48).
    frame.main.windows.push(Window {
        left: 2,
        top: 2,
        width: 5,
        height: 4,
        palette: 2,
        fill: 15,
        frame: Some(WindowFrame {
            base_tile: 0,
            palette: 4,
            dialogue: true,
        }),
        ..Window::default()
    });
    let [plain, _] = render(&frame, &store);
    let v = |screen: &ScreenBuffer, x: usize, y: usize| screen.pixel(x, y)[0];
    let border = |tile: u8| 64 + (tile % 15 + 1);
    for y in 16..48 {
        // Two left columns, the fill across exactly width·8 pixels,
        // then the three right columns and the backdrop.
        assert_eq!(v(&plain, 0, y), border(6), "row {y}: column +6");
        assert_eq!(v(&plain, 8, y), border(7), "row {y}: column +7");
        for x in 16..56 {
            assert_eq!(v(&plain, x, y), 47, "({x}, {y}): the fill, bank 2");
        }
        assert_eq!(
            v(&plain, 56, y),
            border(9),
            "row {y}: column +9 starts at the fill's edge"
        );
        assert_eq!(v(&plain, 64, y), border(10), "row {y}: column +10");
        assert_eq!(v(&plain, 72, y), border(11), "row {y}: column +11");
        assert_eq!(
            v(&plain, 80, y),
            0,
            "row {y}: nothing past the third column"
        );
    }
    for (x, column) in [(0, 0), (8, 1), (16, 2), (48, 2), (56, 3), (64, 4), (72, 5)] {
        assert_eq!(v(&plain, x, 8), border(column), "top row column {column}");
        assert_eq!(
            v(&plain, x, 48),
            border(12 + column),
            "bottom row column {column}"
        );
    }

    // The arrow block: tiles (8..10, 4..6) → pixels (64..80, 32..48).
    let mut renders = Vec::new();
    for index in 0..4u8 {
        frame.main.windows[0].arrow = Some(WindowArrow {
            base_tile: 0,
            index,
        });
        let [main, _] = render(&frame, &store);
        let offset = ARROW_TILE_OFFSETS[usize::from(index)];
        for (pos, (x, y)) in [(64, 32), (72, 32), (64, 40), (72, 40)]
            .into_iter()
            .enumerate()
        {
            assert_eq!(
                v(&main, x, y),
                border(18 + offset * 4 + pos as u8),
                "index {index} pos {pos}: tile 18 + {offset}·4 + {pos} at the border's bank"
            );
        }
        // Only those four tiles change: the column above the block
        // and the +9 column beside it are still border.
        assert_eq!(
            v(&main, 64, 24),
            border(10),
            "index {index}: +10 above the block"
        );
        assert_eq!(
            v(&main, 56, 40),
            border(9),
            "index {index}: +9 beside the block"
        );
        assert_eq!(v(&main, 80, 40), 0, "index {index}: nothing past the block");
        renders.push(main);
    }
    assert_eq!(
        renders[3].as_rgba(),
        renders[1].as_rgba(),
        "index 3 walks back through offset 1: identical to index 1"
    );
    assert_ne!(renders[0].as_rgba(), renders[1].as_rgba());
    assert_ne!(renders[1].as_rgba(), renders[2].as_rgba());

    // The focus icon lands at window x (5-3)·8 = 16 → screen 32..56,
    // rows 16..48: inside the fill, up to but not over the border.
    frame.main.windows[0].focus = Some(WindowFocus {
        asset: focus_gfx,
        index: 1,
        scroll: 0,
    });
    let [main, _] = render(&frame, &store);
    let focus_tile = |t: u8| 32 + (t % 15 + 1); // bank 2
    assert_eq!(v(&main, 31, 16), 47, "left of the icon: the fill");
    assert_eq!(v(&main, 32, 16), focus_tile(12), "frame 1's tile 0");
    assert_eq!(
        v(&main, 55, 47),
        focus_tile(23),
        "frame 1's tile 11 ends at the fill's edge"
    );
    assert_eq!(
        v(&main, 56, 16),
        border(9),
        "the border past the icon is untouched"
    );
    for (pos, (x, y)) in [(64, 32), (72, 32), (64, 40), (72, 40)]
        .into_iter()
        .enumerate()
    {
        assert_eq!(
            v(&main, x, y),
            border(18 + ARROW_TILE_OFFSETS[3] * 4 + pos as u8),
            "the arrow block coexists with the icon"
        );
    }
}

#[test]
fn blend_brightness_only_changes_the_displayed_target_plane() {
    let mut store = FixtureStore::new();
    let (mut frame, _) = window_frame(&mut store);
    frame.main.blend = Blend {
        effect: BlendEffect::BrightnessUp,
        plane1: plane::BG0,
        evy: 16,
        ..Blend::default()
    };
    let [main, _] = render(&frame, &store);
    assert_eq!(main.pixel(16, 16), [255; 4]);
    assert_eq!(main.pixel(0, 0), [0, 0, 0, 255], "untargeted backdrop");
    frame.main.blend.effect = BlendEffect::BrightnessDown;
    let [main, _] = render(&frame, &store);
    assert_eq!(main.pixel(16, 16), [0, 0, 0, 255]);
}

// ---------------------------------------------------------------------------
// The field (3D) plane as engine A's BG0
// ---------------------------------------------------------------------------

mod field_compositing {
    use super::*;
    use apricorn_core::field::{
        FieldScene,
        model::{Mesh, Texture, Vertex},
    };
    use apricorn_gfx::field::tile_position;
    use std::sync::Arc;

    /// The bedroom's camera preset (ov01 preset 4) as core decodes it;
    /// the shim renders synthetic scenes with its own indoor constant,
    /// so this only fills the scene's data field.
    fn bedroom_camera() -> apricorn_core::field::ov01::CameraPreset {
        apricorn_core::field::ov01::CameraPreset {
            distance: 0x0061_B89B,
            angle: [0xDC82, 0, 0],
            padding: 0,
            perspective_type: 1,
            fovy_angle: 0x0281,
            near: 0x0009_6000,
            far: 0x006C_7000,
            look_at_offset: [0; 3],
        }
    }

    /// A synthetic field: one red ground quad, `half` world units to
    /// each side of the player's tile (0, 0), `alpha` 0–31, and an
    /// invisible (alpha 0) player texture so the static view's one
    /// billboard draws nothing.
    fn field(half: i32, alpha: u8) -> apricorn_core::frame::FieldFrame {
        apricorn_core::frame::FieldFrame::static_scene(field_scene(half, alpha))
    }

    fn field_scene(half: i32, alpha: u8) -> Arc<FieldScene> {
        let centre = tile_position([0, 0]);
        let h = half * 4096;
        let corner = |dx: i32, dz: i32| Vertex {
            position: [centre[0] + dx, 0, centre[2] + dz],
            gx_position: [centre[0] + dx, 0, centre[2] + dz],
            uv: [0, 0],
            color: 0x7FFF,
            normal: None,
        };
        let (a, b, c, d) = (corner(-h, -h), corner(h, -h), corner(h, h), corner(-h, h));
        Arc::new(FieldScene::synthetic(
            0,
            vec![Mesh {
                gx_transform: apricorn_core::field::model::GxTransform::IDENTITY,
                primitives: vec![apricorn_core::field::model::Primitive::Quad([a, b, c, d])],
                texture: Some(Arc::new(Texture {
                    width: 1,
                    height: 1,
                    pixels: vec![[255, 0, 0, 255]],
                    format: apricorn_core::formats::TexFmt::Pltt256,
                    color0_transparent: false,
                    raw: None,
                })),
                texture_flags: 0,
                alpha,
                // These fixtures exercise 2D composition, not face culling.
                // A zero face mask correctly hides a nondegenerate GX quad.
                polygon_attr: (u32::from(alpha) << 16) | (3 << 6),
                diffuse_ambient: 0x7FFF,
                specular_emission: 0,
            }],
            Texture {
                width: 1,
                height: 1,
                pixels: vec![[0, 0, 0, 0]],
                format: apricorn_core::formats::TexFmt::Pltt256,
                color0_transparent: true,
                raw: None,
            },
            [0, 0],
            bedroom_camera(),
        ))
    }

    /// The field (BG0, enabled) under an opaque gray BG1 (value 5
    /// everywhere), both at priority 0 until a test says otherwise.
    fn scene(store: &mut FixtureStore) -> LogicalFrame {
        let tiles = store.add_tiles(0, &[5u8; 64]);
        let screen = store.add_screen(8, 8, &[entry(0, false, false, 0)]);
        let palette = store.add_palette(true, &gray_palette(16));
        let mut frame = LogicalFrame {
            main: engine(
                &[BgLayer::default(), bg_layer(screen)],
                &[(0, tiles)],
                palette,
            ),
            ..LogicalFrame::default()
        };
        frame.main.field = Some(field(100, 31));
        frame.main.bgs[0].enabled = true;
        frame
    }

    #[test]
    fn the_field_is_bg0_and_sorts_by_priority() {
        let mut store = FixtureStore::new();
        let mut frame = scene(&mut store);
        // BG1 at priority 0 above the field at priority 1.
        frame.main.bgs[0].priority = 1;
        frame.main.bgs[1].priority = 0;
        let [main, _] = render(&frame, &store);
        assert_eq!(main.pixel(128, 96), [5, 5, 5, 255], "BG1 covers the field");
        // The field on top: red where the quad lands, BG1 elsewhere.
        frame.main.bgs[0].priority = 0;
        frame.main.bgs[1].priority = 1;
        let [main, _] = render(&frame, &store);
        assert_eq!(main.pixel(128, 96), [255, 0, 0, 255], "the field shows");
        assert_eq!(
            main.pixel(0, 0),
            [5, 5, 5, 255],
            "uncovered 3D pixels are transparent"
        );
        // A tie goes to the lower BG index — BG0, the field.
        frame.main.bgs[1].priority = 0;
        assert_eq!(render(&frame, &store)[0].pixel(128, 96), [255, 0, 0, 255]);
    }

    #[test]
    fn a_disabled_bg0_or_the_hardware_window_hides_the_field() {
        let mut store = FixtureStore::new();
        let mut frame = scene(&mut store);
        frame.main.bgs[1].enabled = false;
        frame.main.backdrop = 0x7C00; // blue
        let [main, _] = render(&frame, &store);
        assert_eq!(main.pixel(128, 96), [255, 0, 0, 255]);
        assert_eq!(main.pixel(0, 0), [0, 0, 255, 255], "backdrop where clear");
        frame.main.bgs[0].enabled = false;
        assert_eq!(render(&frame, &store)[0].pixel(128, 96), [0, 0, 255, 255]);
        frame.main.bgs[0].enabled = true;
        frame.main.bgs[0].hidden_rect = Some((120, 90, 136, 102));
        let [main, _] = render(&frame, &store);
        assert_eq!(
            main.pixel(128, 96),
            [0, 0, 255, 255],
            "hidden inside the rect"
        );
        assert_eq!(main.pixel(140, 96), [255, 0, 0, 255], "shown outside it");
    }

    #[test]
    fn sprites_composite_above_the_field_by_priority_and_win_ties() {
        let mut store = FixtureStore::new();
        let mut frame = scene(&mut store);
        frame.main.bgs[1].enabled = false;
        frame.main.bgs[0].priority = 1;
        // The OAM fixture: an 8×8 cell whose pixel value is its column
        // (0 transparent), OBJ palette bank 1, placed so it lands on
        // the field around (126..134, 92..100).
        let tiles = store.add_tiles(0, &(0..64).map(|i| (i % 8) as u8).collect::<Vec<_>>());
        let obj_palette = store.add_palette(true, &gray_palette(32));
        let mut sprite = store.add_sprite(tiles, obj_palette);
        sprite.x = 128;
        sprite.y = 96;
        frame.main.sprites.push(sprite);
        let [main, _] = render(&frame, &store);
        assert_eq!(main.pixel(127, 93), [17, 17, 17, 255], "OBJ over the field");
        assert_eq!(
            main.pixel(126, 93),
            [255, 0, 0, 255],
            "OBJ value 0 reveals the field"
        );
        frame.main.sprites[0].priority = 1;
        assert_eq!(
            render(&frame, &store)[0].pixel(127, 93)[0],
            17,
            "OBJ wins the tie"
        );
        frame.main.bgs[0].priority = 0;
        assert_eq!(
            render(&frame, &store)[0].pixel(127, 93),
            [255, 0, 0, 255],
            "a higher-priority field hides the OBJ"
        );
    }

    #[test]
    fn master_brightness_and_fades_apply_to_the_field() {
        let mut store = FixtureStore::new();
        let mut frame = scene(&mut store);
        frame.main.bgs[1].enabled = false;
        frame.main.brightness = MasterBrightness {
            mode: BrightnessMode::Down,
            value: 8,
        };
        let [main, _] = render(&frame, &store);
        let down = |c: u32| (c - (c * 8 >> 4)) as u8;
        assert_eq!(main.pixel(128, 96), [down(255), 0, 0, 255]);
        // The blend unit's fade on BG0 as a first target (no second
        // target below), then the master brightness on top of it:
        // red 255 stays, the other channels rise to 255 · 8/16 = 127,
        // then all three halve.
        frame.main.blend = Blend {
            plane1: plane::BG0,
            effect: BlendEffect::BrightnessUp,
            evy: 8,
            ..Blend::default()
        };
        let [main, _] = render(&frame, &store);
        let up: u32 = 255 * 8 >> 4;
        assert_eq!(main.pixel(128, 96), [down(255), down(up), down(up), 255]);
    }

    #[test]
    fn translucent_field_pixels_blend_with_the_second_target_by_their_alpha() {
        let mut store = FixtureStore::new();
        let mut frame = scene(&mut store);
        frame.main.field = Some(field(100, 15));
        frame.main.bgs[0].priority = 0;
        frame.main.bgs[1].priority = 1;
        // No second target: the pixel shows as is, whatever the mode.
        let [main, _] = render(&frame, &store);
        assert_eq!(main.pixel(128, 96), [255, 0, 0, 255]);
        // BG1 as a second target: ColorBlend5 with eva = 16, evb = 16
        // and the + 0x10 rounding term, regardless of the effect mode
        // or first-target mask. The blue channel pins the rounding:
        // (0 · 16 + 5 · 16 + 16) >> 5 = 3, where an unrounded shift
        // would give 2.
        frame.main.blend = Blend {
            plane2: plane::BG1,
            ..Blend::default()
        };
        let [main, _] = render(&frame, &store);
        let mix = |a: u32, b: u32| ((a * 16 + b * 16 + 0x10) >> 5) as u8;
        assert_eq!(mix(0, 5), 3);
        assert_eq!(
            main.pixel(128, 96),
            [mix(255, 5), mix(0, 5), mix(0, 5), 255]
        );
        // An opaque field pixel over a second target is unchanged even
        // under an alpha effect with EVA/EBV set — the 3D plane never
        // uses the register weights.
        frame.main.field = Some(field(100, 31));
        frame.main.blend = Blend {
            plane1: plane::BG0,
            effect: BlendEffect::Alpha,
            plane2: plane::BG1,
            eva: 8,
            ebv: 8,
            evy: 0,
        };
        assert_eq!(render(&frame, &store)[0].pixel(128, 96), [255, 0, 0, 255]);
        // The backdrop completes the search too.
        frame.main.field = Some(field(100, 15));
        frame.main.bgs[1].enabled = false;
        frame.main.backdrop = 0x7C00; // blue
        frame.main.blend = Blend {
            plane2: plane::BD,
            ..Blend::default()
        };
        assert_eq!(
            render(&frame, &store)[0].pixel(128, 96),
            [mix(255, 0), 0, mix(0, 255), 255]
        );
    }
}

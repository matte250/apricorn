//! The rasterizer — one logical frame to two RGBA screens.
//!
//! For each engine ([`EngineFrame`]) this composes the 256-color BG
//! palette RAM from the frame's [`PaletteLoad`]s in order (later
//! loads overwrite earlier ones, exactly the hardware's single RAM),
//! then walks every output pixel and composites the enabled text BG
//! layers top-down by `BGxCNT` priority (ties to the lower BG index,
//! the GBA convention), over the backdrop color. A layer's pixel comes
//! from its screen entry at the scrolled position (scroll wraps
//! within the layer's map size), the 8×8 tile that entry names —
//! resolved against the char-block slot's [`TilePlacement`]s, the
//! later placement winning an overlap — and the composed palette RAM:
//! 4bpp entries pick a 16-color bank, 8bpp addresses the RAM
//! directly, and color 0 is transparent (`docs/nds-2d.md`, "Text-layer
//! screen entries" and "Palettes").
//!
//! A layer's message [`Window`]s are composited *into that layer*:
//! their content stands in for the screen map within the rect (the
//! fill, the printed glyphs, the cumulative scroll), the 9-slice
//! frame around it, and the wait arrow one tile past its corner —
//! the tilemap entries the original's window-to-VRAM copy produces,
//! re-derived from the printing state at raster time. Window pixels
//! belong to their owning layer's plane: they blend and prioritize
//! as that layer does.
//!
//! Compositing is exact and integer end to end:
//!
//! * **Alpha blend** (`BLDCNT`/`BLDALPHA`): if the *displayed*
//!   (topmost) pixel's plane is in the first-target mask and the
//!   effect is alpha, the rasterizer looks below it for the topmost
//!   pixel whose plane is in the second-target mask — the backdrop
//!   is the bottom-most plane — and emits
//!   `(first·EVA + second·EBV + 8) >> 4` per channel, clamped. The
//!   5-bit register weights **clamp to 16** and the `+ 8` is the
//!   round-to-nearest half-step, both exactly as melonDS's software
//!   path (the future harness's comparison target). With no second
//!   target the first target shows unblended; a topmost pixel outside
//!   the first-target mask always passes through unblended.
//! * **Master brightness** applies last, to the whole screen:
//!   up is `c + (255 - c)·value >> 4`, down is `c - c·value >> 4`
//!   (the register's 5-bit weight, value 16 reaches the limit).
//!
//! Intro OBJ cells are rasterized with their own palettes and OAM
//! ordering, winning BG priority ties.
//!
//! * **The 3D plane** (`GX_BG0_AS_3D`): when the engine carries a
//!   [`EngineFrame::field`], BG0 *is* the 3D core's output —
//!   [`crate::field::render`] draws it first (opaque, billboards,
//!   translucent; alpha 0 where nothing covered the pixel) and the
//!   compositor samples it in BG0's place with BG0's `priority`,
//!   `enabled` and `hidden_rect`, ignoring BG0's tilemap and windows.
//!   A 3D pixel blends with the topmost second-target pixel below it
//!   by its *own* alpha — `(first · (a + 1) + second · (32 − a − 1)
//!   + 16) >> 5`, melonDS's `ColorBlend5` — whenever `BLDCNT` names that
//!   plane a second target, regardless of the effect mode or the
//!   first-target mask (an opaque pixel, `a = 31`, comes through
//!   unchanged); without a second target the brightness effects apply
//!   to it as to any first-target plane. OBJ pixels still win priority
//!   ties over it, and master brightness applies last.
//!
//! No VRAM banking exists — layers name char-block slots. A layer
//! whose referenced asset is missing renders transparent. Dialogue arrows contain
//! the original border pixels under their ink; clearing an arrow
//! restores that border. The glyph row-doubling table is the
//! all-rows flag the boot flow uses, not per-row bits.
//! The {YESNO} focus indicator samples top-most in its reserved
//! column (pret's blit order is print order; no shipped message
//! prints into the column).

use apricorn_core::assets::AssetStore;
use apricorn_core::cache;
use apricorn_core::font::Font;
use apricorn_core::frame::{
    ARROW_TILE_OFFSETS, AssetId, BgLayer, BlendEffect, BrightnessMode, ColorMode, EngineFrame,
    LogicalFrame, TilePlacement, TilemapEdit, Window, plane,
};

/// The handle-resolution seam between the asset store and the
/// rasterizer.
///
/// The store owns the decoded chunks; the rasterizer only needs to
/// resolve a frame's [`AssetId`] handles back to them. Tests bring
/// their own implementation with hand-built chunks, and the
/// [`AssetStore`] itself implements this, so ROM loading and pixel
/// math stay separable.
pub trait AssetSource {
    /// The tiles chunk a `char_base` slot or tile handle names.
    fn tiles(&self, id: AssetId) -> Option<&cache::Tiles>;
    /// The screen chunk a layer's `screen` handle names.
    fn screen(&self, id: AssetId) -> Option<&cache::Screen>;
    /// The placed palette colors (PMCP applied) a palette load names.
    fn placed_palette(&self, id: AssetId) -> Option<&[[u8; 4]]>;
    /// The font a window glyph prints from.
    fn font(&self, id: AssetId) -> Option<&Font>;
    /// Sprite geometry, if this source provides it.
    fn cells(&self, _id: AssetId) -> Option<&cache::Cells> {
        None
    }
    /// Sprite animations, if this source provides them.
    fn animation(&self, _id: AssetId) -> Option<&cache::Animation> {
        None
    }
}

impl AssetSource for AssetStore {
    fn cells(&self, id: AssetId) -> Option<&cache::Cells> {
        AssetStore::cells(self, id)
    }
    fn animation(&self, id: AssetId) -> Option<&cache::Animation> {
        AssetStore::animation(self, id)
    }
    fn tiles(&self, id: AssetId) -> Option<&cache::Tiles> {
        AssetStore::tiles(self, id)
    }

    fn screen(&self, id: AssetId) -> Option<&cache::Screen> {
        AssetStore::screen(self, id)
    }

    fn placed_palette(&self, id: AssetId) -> Option<&[[u8; 4]]> {
        AssetStore::placed_palette(self, id)
    }

    fn font(&self, id: AssetId) -> Option<&Font> {
        AssetStore::font(self, id)
    }
}

/// One engine's rendered screen: 256×192 pixels of RGBA8 (red,
/// green, blue, alpha; alpha is always 255 — transparency is a
/// compositing fact, not an output value).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScreenBuffer {
    pixels: Box<[[u8; 4]]>,
}

impl ScreenBuffer {
    /// The screen width in pixels.
    pub const WIDTH: usize = 256;
    /// The screen height in pixels.
    pub const HEIGHT: usize = 192;

    /// A black screen.
    #[must_use]
    pub fn new() -> Self {
        Self {
            pixels: vec![[0, 0, 0, 255]; Self::WIDTH * Self::HEIGHT].into_boxed_slice(),
        }
    }

    /// The pixel at `(x, y)`.
    ///
    /// # Panics
    /// Panics when `x`/`y` fall outside the screen.
    #[must_use]
    pub fn pixel(&self, x: usize, y: usize) -> [u8; 4] {
        self.pixels[self.index(x, y)]
    }

    /// The pixels as a flat RGBA8 row-major image, ready for a
    /// texture upload.
    #[must_use]
    pub fn as_rgba(&self) -> &[[u8; 4]] {
        &self.pixels
    }

    fn index(&self, x: usize, y: usize) -> usize {
        assert!(x < Self::WIDTH && y < Self::HEIGHT, "pixel off the screen");
        y * Self::WIDTH + x
    }
}

impl Default for ScreenBuffer {
    fn default() -> Self {
        Self::new()
    }
}

/// Renders both engines' screens for `frame` — engine A (MAIN) then
/// engine B (SUB), in that order. The rasterizer is display-agnostic:
/// [`LogicalFrame::display`] says which LCD each drives, and the
/// caller (presenter, dump tool) maps them.
#[must_use]
pub fn render<S: AssetSource + ?Sized>(frame: &LogicalFrame, store: &S) -> [ScreenBuffer; 2] {
    [
        render_engine(&frame.main, store),
        render_engine(&frame.sub, store),
    ]
}

/// Renders one engine: 256×192 pixels composited from its BG layers
/// (and their windows), blend unit, backdrop, and brightness.
fn render_engine<S: AssetSource + ?Sized>(engine: &EngineFrame, store: &S) -> ScreenBuffer {
    // The draw order: lower priority on top, ties to the lower BG
    // index. OBJ pixels join this order by priority, winning BG ties;
    // the backdrop is handled as the bottom-most plane below.
    let mut order: [usize; 4] = [0, 1, 2, 3];
    order.sort_by_key(|&i| (engine.bgs[i].priority, i));
    let palette = compose_palette(engine, store);
    let objects = crate::sprites::rasterize(engine, store);
    // The 3D core's output stands in for BG0's tilemap while a field
    // is bound (GX_BG0_AS_3D, fieldmap.c:514); the plane's alpha is 0
    // where no polygon covered the pixel (the clear colour's alpha).
    let field = engine.field.as_ref().map(|field| {
        let view = crate::field::scene_view(field);
        let mut buffer = crate::gx3d::Gx3dBuffer::new();
        let registers = crate::field::field_registers(&view);
        crate::field::render_view_gx(&view, &registers, &mut buffer);
        buffer.colors().to_vec()
    });

    let mut out = ScreenBuffer::new();
    for y in 0..ScreenBuffer::HEIGHT {
        for x in 0..ScreenBuffer::WIDTH {
            let [r, g, b, _] = composite_pixel(
                engine,
                store,
                &palette,
                &order,
                field.as_deref(),
                objects[y * 256 + x],
                x,
                y,
            );
            out.pixels[out.index(x, y)] = [r, g, b, 255];
        }
    }
    apply_brightness(engine, &mut out);
    out
}

/// Composes the engine's 256-color BG palette RAM — the hardware's
/// single RAM every 4bpp bank, 8bpp pixel, and window content of this
/// engine decodes against.
fn compose_palette<S: AssetSource + ?Sized>(engine: &EngineFrame, store: &S) -> Vec<[u8; 4]> {
    // Power-on RAM is zeroed BGR555 (black); loads land in order and
    // later loads overwrite earlier ones.
    let mut ram = vec![[0u8, 0, 0, 255]; 256];
    for load in &engine.palette_loads {
        let Some(palette) = store.placed_palette(load.asset) else {
            continue;
        };
        let start = usize::from(load.offset);
        for (i, &color) in palette.iter().take(usize::from(load.colors)).enumerate() {
            if let Some(slot) = ram.get_mut(start + i) {
                *slot = color;
            }
        }
    }
    for &(index, color) in &engine.palette_overrides {
        if let Some(slot) = ram.get_mut(usize::from(index)) {
            *slot = backdrop_rgba(color);
        }
    }
    ram
}

/// Composites one pixel: the topmost opaque layer pixel (by draw
/// order) or the backdrop, alpha-blended per the engine's blend unit.
/// `field` is the rendered 3D plane standing in for BG0, if bound.
#[allow(clippy::too_many_arguments)]
fn composite_pixel<S: AssetSource + ?Sized>(
    engine: &EngineFrame,
    store: &S,
    palette: &[[u8; 4]],
    order: &[usize; 4],
    field: Option<&[[u8; 4]]>,
    object: Option<crate::sprites::Pixel>,
    x: usize,
    y: usize,
) -> [u8; 4] {
    let blending = matches!(engine.blend.effect, BlendEffect::Alpha) && engine.blend.plane1 != 0;
    // The displayed pixel (topmost opaque), and — when it is a blend
    // first target — the second target found below it.
    let mut top: Option<([u8; 4], bool, u8)> = None;
    let mut second: Option<[u8; 4]> = None;
    // The topmost pixel's 5-bit alpha when it is the 3D plane's: such
    // a pixel always looks for a second target and blends by this.
    let mut three_d: Option<u8> = None;

    let object_slot = object.map(|obj| {
        order
            .iter()
            .position(|&i| engine.bgs[i].priority >= obj.priority)
            .unwrap_or(4)
    });
    for slot in 0..4 + usize::from(object.is_some()) {
        let (color, bit, semi, is_3d) = if object_slot == Some(slot) {
            let obj = object.expect("the inserted OBJ pixel");
            (obj.color, plane::OBJ, obj.semi, false)
        } else {
            let i = order[slot - usize::from(object_slot.is_some_and(|at| at < slot))];
            let layer = &engine.bgs[i];
            if !layer.enabled {
                continue;
            }
            let Some(color) = sample_layer(engine, store, palette, field, i, layer, x, y) else {
                continue;
            };
            (color, plane::BG0 << i, false, i == 0 && field.is_some())
        };
        if top.is_none() {
            if is_3d {
                three_d = Some(alpha5(color[3]));
            }
            let blend_top = semi || is_3d || (blending && engine.blend.plane1 & bit != 0);
            top = Some((color, blend_top, bit));
            // A topmost pixel outside plane1 passes through
            // unblended — nothing below can change it.
            if !blend_top {
                break;
            }
        } else if second.is_none() && engine.blend.plane2 & bit != 0 {
            second = Some(color);
            break;
        }
    }

    let backdrop = backdrop_rgba(engine.backdrop);
    let (mut color, blend_top, bit) = match top {
        Some(top) => top,
        None => (backdrop, false, plane::BD),
    };
    // The backdrop is the bottom-most plane: it completes a second
    // target search (as plane BD) but never starts one here — there
    // is nothing below it to blend with.
    if blend_top && second.is_none() && engine.blend.plane2 & plane::BD != 0 {
        second = Some(backdrop);
    }
    // The 3D plane: melonDS's ColorComposite effect 5 when a second
    // target lies below, else the plain first-target effects.
    if let Some(alpha) = three_d {
        return match second {
            Some(second) => blend_3d(color, second, alpha),
            None => plane_brightness(engine, color, bit),
        };
    }
    if !blend_top {
        color = plane_brightness(engine, color, bit);
    }

    match (blend_top, second) {
        (true, Some(second)) => alpha_blend(color, second, engine.blend.eva, engine.blend.ebv),
        _ => color,
    }
}

/// The blend unit's brightness fades (`BLDCNT` effects 2 and 3) on a
/// first-target plane's pixel; other effects leave it alone.
fn plane_brightness(engine: &EngineFrame, mut color: [u8; 4], bit: u8) -> [u8; 4] {
    if engine.blend.plane1 & bit == 0 {
        return color;
    }
    let weight = u32::from(engine.blend.evy.min(16));
    for channel in &mut color[..3] {
        let c = u32::from(*channel);
        *channel = match engine.blend.effect {
            BlendEffect::BrightnessUp => (c + ((255 - c) * weight >> 4)) as u8,
            BlendEffect::BrightnessDown => (c - (c * weight >> 4)) as u8,
            _ => *channel,
        };
    }
    color
}

/// An 8-bit alpha back to the 3D core's 5 bits, rounded.
fn alpha5(alpha: u8) -> u8 {
    ((u32::from(alpha) * 31 + 127) / 255) as u8
}

/// The 3D plane's own alpha blend — melonDS `GPU2D_Soft.h:81-96`
/// `ColorBlend5`: `eva = a + 1`, `evb = 32 − eva`, per channel
/// `(first · eva + second · evb + 0x10) >> 5`, clamped — the `+ 0x10`
/// is half the divisor, so the mix rounds to nearest like the
/// register blend's `+ 8) >> 4` in [`alpha_blend`]; in 8-bit channels
/// here where melonDS works in 6.
fn blend_3d(first: [u8; 4], second: [u8; 4], alpha: u8) -> [u8; 4] {
    let eva = u32::from(alpha.min(31)) + 1;
    let evb = 32 - eva;
    let mix = |a: u8, b: u8| {
        let v = (u32::from(a) * eva + u32::from(b) * evb + 0x10) >> 5;
        u8::try_from(v.min(0xFF)).expect("clamped to u8")
    };
    [
        mix(first[0], second[0]),
        mix(first[1], second[1]),
        mix(first[2], second[2]),
        255,
    ]
}

/// Samples one layer at output pixel `(x, y)` — `None` when the
/// layer is transparent there (color 0, a missing asset, or a tile
/// index beyond the char block). The layer's windows stand in for
/// the screen map wherever they cover. With a `field` bound, BG0 is
/// the 3D plane: its rendered pixel (alpha 0 is transparent) replaces
/// the tilemap, windows and edits, after the hardware window.
#[allow(clippy::too_many_arguments)]
fn sample_layer<S: AssetSource + ?Sized>(
    engine: &EngineFrame,
    store: &S,
    palette: &[[u8; 4]],
    field: Option<&[[u8; 4]]>,
    layer_index: usize,
    layer: &BgLayer,
    x: usize,
    y: usize,
) -> Option<[u8; 4]> {
    // The scrolled position, wrapped within the layer's map.
    if let Some((left, top, right, bottom)) = layer.hidden_rect {
        if x >= usize::from(left)
            && x < usize::from(right)
            && y >= usize::from(top)
            && y < usize::from(bottom)
        {
            return None;
        }
    }
    if layer_index == 0 {
        if let Some(field) = field {
            let pixel = field[y * ScreenBuffer::WIDTH + x];
            return (pixel[3] != 0).then_some(pixel);
        }
    }
    let map_w = usize::from(layer.size.tiles_wide()) * 8;
    let map_h = usize::from(layer.size.tiles_tall()) * 8;
    let mx = (x + usize::from(layer.scroll_x)) % map_w;
    let my = (y + usize::from(layer.scroll_y)) % map_h;

    // The windows print on top of the screen map, in model order —
    // later windows draw over earlier ones, as tilemap writes do.
    for window in engine.windows.iter().rev() {
        if window.bg as usize == layer_index {
            let (left, top, width, height) = window.rect_px();
            if mx >= usize::from(left)
                && mx < usize::from(left + width)
                && my >= usize::from(top)
                && my < usize::from(top + height)
            {
                // Window tilemap entries replace this layer's old map, even
                // when the resulting pixel is transparent to lower planes.
                return sample_window(engine, store, palette, window, mx, my);
            }
            if let Some(color) = sample_window(engine, store, palette, window, mx, my) {
                return Some(color);
            }
        }
    }

    let screen_id = layer.screen?;
    let screen = store.screen(screen_id)?;

    // The screen chunk covers the map from its top-left; tiles the
    // chunk does not cover read as entry 0 (the plan's rule for
    // maps shorter than the 256×256 layer).
    let chunk_w = usize::from(screen.width() / 8);
    let chunk_h = usize::from(screen.height() / 8);
    let (tx, ty) = (mx / 8, my / 8);
    let entry = if tx < chunk_w && ty < chunk_h {
        let at = (ty * chunk_w + tx) * 2;
        let entries = screen.entries();
        if at + 1 >= entries.len() {
            return None;
        }
        u16::from_le_bytes([entries[at], entries[at + 1]])
    } else {
        0
    };

    let mut tile = usize::from(entry & 0x3FF);
    let mut h_flip = entry & 0x0400 != 0;
    let mut v_flip = entry & 0x0800 != 0;
    let mut bank = usize::from(entry >> 12);

    // The tilemap-buffer rewrites made after the screen load, in push
    // order — a later edit wins, exactly as the buffer's last write
    // does. Edits name map-space tiles, so they cover tiles outside
    // the screen chunk too (those read as entry 0 above).
    for edit in &engine.tilemap_edits {
        let (bg, left, top, width, height) = match *edit {
            TilemapEdit::Palette {
                bg,
                left,
                top,
                width,
                height,
                ..
            }
            | TilemapEdit::Fill {
                bg,
                left,
                top,
                width,
                height,
                ..
            } => (bg, left, top, width, height),
        };
        if bg as usize != layer_index {
            continue;
        }
        let (left, top, width, height) = (
            usize::from(left),
            usize::from(top),
            usize::from(width),
            usize::from(height),
        );
        if tx < left || tx >= left + width || ty < top || ty >= top + height {
            continue;
        }
        match *edit {
            TilemapEdit::Palette {
                bank: edit_bank, ..
            } => bank = usize::from(edit_bank),
            TilemapEdit::Fill {
                tile: edit_tile,
                palette,
                ..
            } => {
                tile = usize::from(edit_tile);
                bank = usize::from(palette);
                h_flip = false;
                v_flip = false;
            }
        }
    }

    let mut px = mx % 8;
    let mut py = my % 8;
    if h_flip {
        px = 7 - px;
    }
    if v_flip {
        py = 7 - py;
    }
    let (tiles, local) = resolve_tile(store, &engine.char_blocks[layer_index_char(layer)], tile)?;
    if local >= usize::try_from(tiles.tile_count()).expect("u32 fits usize") {
        return None;
    }
    let value = tiles.pixels()[local * 64 + py * 8 + px];
    if value == 0 {
        return None; // color 0 is transparent
    }
    let index = match layer.color_mode {
        ColorMode::Bpp4 => bank * 16 + usize::from(value),
        ColorMode::Bpp8 => usize::from(value),
    };
    palette.get(index).copied()
}

/// The char-block slot a layer names — `char_base` is 0–7 in the
/// model, matching the slot array.
fn layer_index_char(layer: &BgLayer) -> usize {
    layer.char_base as usize & 7
}

/// Resolves tile index `tile` against one char-block slot's
/// placements — the placement that covers it wins, later placements
/// over earlier ones (VRAM loads overwrite).
fn resolve_tile<'a, S: AssetSource + ?Sized>(
    store: &'a S,
    block: &[TilePlacement],
    tile: usize,
) -> Option<(&'a cache::Tiles, usize)> {
    block.iter().rev().find_map(|placement| {
        let tiles = store.tiles(placement.asset)?;
        let local = tile as i64 - i64::from(placement.tile);
        let count = i64::from(tiles.tile_count());
        if (0..count).contains(&local) {
            Some((tiles, local as usize))
        } else {
            None
        }
    })
}

/// Samples one window at map pixel `(mx, my)` — `None` where the
/// window (and its frame and arrow) is transparent there, falling
/// through to the screen map beneath.
fn sample_window<S: AssetSource + ?Sized>(
    engine: &EngineFrame,
    store: &S,
    palette: &[[u8; 4]],
    window: &Window,
    mx: usize,
    my: usize,
) -> Option<[u8; 4]> {
    let (rx, ry, rw, rh) = window.rect_px();
    let (rx, ry, rw, rh) = (
        usize::from(rx),
        usize::from(ry),
        usize::from(rw),
        usize::from(rh),
    );

    // The interior: the fill, overprinted by the glyphs (a glyph's
    // non-zero levels overwrite the buffer; its zero levels leave
    // whatever earlier glyphs or the fill left).
    if mx >= rx && mx < rx + rw && my >= ry && my < ry + rh {
        return sample_window_interior(store, palette, window, mx - rx, my - ry);
    }

    // The wait arrow: 2×2 tiles one tile right of the bottom-right
    // corner (`TextPrinter_DrawDownArrow`). Palette argument 0x10
    // preserves the dialogue border's bank rather than selecting bank 1.
    if let Some(arrow) = window.arrow {
        let ax = usize::from(window.left) + usize::from(window.width) + 1;
        let ay = usize::from(window.top) + usize::from(window.height) - 2;
        let tile_x = mx / 8;
        let tile_y = my / 8;
        if tile_x >= ax && tile_x < ax + 2 && tile_y >= ay && tile_y < ay + 2 {
            let pos = (tile_x - ax) + (tile_y - ay) * 2; // 0..3
            let offset = u32::from(ARROW_TILE_OFFSETS[usize::from(arrow.index) % 4]);
            return sample_block_tile(
                store,
                palette,
                engine,
                window,
                u32::from(arrow.base_tile) + 18 + offset * 4 + pos as u32,
                window.frame.filter(|f| f.dialogue).map_or(1, |f| f.palette),
                mx % 8,
                my % 8,
            );
        }
    }

    // The 9-slice frame: one tile thick around the rect, from
    // (x-1, y-1) — DrawFrameAndWindow's border, entries at the
    // frame's palette bank.
    if let Some(frame) = window.frame {
        let tile_x = mx / 8;
        let tile_y = my / 8;
        let ctx = tile_x as i32 - i32::from(window.left);
        let cty = tile_y as i32 - i32::from(window.top);
        let (wide, tall) = (i32::from(window.width), i32::from(window.height));
        if frame.dialogue && (-2..=wide + 2).contains(&ctx) && (-1..=tall).contains(&cty) {
            // render_window.s sub_0200E6B4: six columns per row.
            // The interior column repeats across the window's width.
            let column = if ctx < 0 {
                ctx + 2
            } else if ctx >= wide {
                ctx - wide + 3
            } else {
                2
            };
            let row = if cty < 0 {
                0
            } else if cty == tall {
                2
            } else {
                1
            };
            return sample_block_tile(
                store,
                palette,
                engine,
                window,
                u32::from(frame.base_tile) + (row * 6 + column) as u32,
                frame.palette,
                mx % 8,
                my % 8,
            );
        }
        if !frame.dialogue && (-1..=wide).contains(&ctx) && (-1..=tall).contains(&cty) {
            let border = if ctx < 0 && cty < 0 {
                0 // TL
            } else if ctx == wide && cty < 0 {
                2 // TR
            } else if cty == tall && ctx < 0 {
                6 // BL
            } else if ctx == wide && cty == tall {
                8 // BR
            } else if cty < 0 {
                1 // top edge
            } else if ctx < 0 {
                3 // left edge
            } else if ctx == wide {
                5 // right edge
            } else if cty == tall {
                7 // bottom edge
            } else {
                // The interior was handled above; the range check
                // leaves nothing else.
                return None;
            };
            return sample_block_tile(
                store,
                palette,
                engine,
                window,
                u32::from(frame.base_tile) + border,
                frame.palette,
                mx % 8,
                my % 8,
            );
        }
    }

    None
}

/// One interior pixel of the window rect at window-relative
/// `(wx, wy)`: the fill unless a glyph's non-zero level covers it.
fn sample_window_interior<S: AssetSource + ?Sized>(
    store: &S,
    palette: &[[u8; 4]],
    window: &Window,
    wx: usize,
    wy: usize,
) -> Option<[u8; 4]> {
    // The scroll shifts the whole pixel buffer up: content at
    // unscrolled row `r` shows at window row `r - scroll`, so the
    // row printed at `wy` is the content at `wy + scroll`.
    let content_y = wy + usize::from(window.scroll);
    let bank = usize::from(window.palette) * 16;

    // The {YESNO} focus indicator: the printer's blit, shifted by
    // any scroll that landed after it (its content rows are
    // `focus.scroll..+32`). Pret's blit order is print order — text
    // printed after the indicator overdraws it — but every shipped
    // message that carries `{YESNO 0}` keeps the 24-pixel column
    // reserved, so sampling it top-most is observationally the same
    // (a deviation `docs/gfx.md` documents).
    if let Some(focus) = window.focus {
        let focus_x = (usize::from(window.width) - 3) * 8;
        let fx = wx as i64 - focus_x as i64;
        let fy = content_y as i64 - i64::from(focus.scroll);
        if (0..24).contains(&fx) && (0..32).contains(&fy) {
            let (fx, fy) = (fx as usize, fy as usize);
            // The focus NCGR is 48 tiles in 384-byte frames of
            // twelve 8×8 4bpp tiles, three per row — pret's blit
            // addresses it tile-major (`GetPixelAddressFromBlit4bpp`).
            let frame = usize::from(focus.index) * 12;
            let tile = frame + (fy / 8) * 3 + fx / 8;
            if let Some(tiles) = store.tiles(focus.asset) {
                let at = tile * 64 + (fy % 8) * 8 + fx % 8;
                if let Some(&value) = tiles.pixels().get(at) {
                    if value != 0 {
                        // The blit's colorKey is 0: the indicator's
                        // own zero pixels stay transparent.
                        return Some(palette[bank + usize::from(value & 0xF)]);
                    }
                }
            }
        }
    }

    for glyph in window.glyphs.iter().rev() {
        let Some(font) = store.font(glyph.font) else {
            continue;
        };
        let g = font.glyph(glyph.glyph);
        let (gx, gy) = (usize::from(glyph.x), usize::from(glyph.y));
        // The copy's source clip: `srcWidth` is the glyph's advance,
        // `srcHeight` its fixed height (doubled by the row table).
        let height = if glyph.double_rows {
            usize::from(g.height) * 2
        } else {
            usize::from(g.height)
        };
        if wx < gx || wx >= gx + usize::from(g.width) || content_y < gy || content_y >= gy + height
        {
            continue;
        }
        let sx = wx - gx;
        let sy = if glyph.double_rows {
            (content_y - gy) / 2
        } else {
            content_y - gy
        };
        let level = g.level(sx, sy);
        if level == 0 {
            continue; // level 0 leaves the buffer (fill or earlier glyph)
        }
        let index = match level {
            1 => glyph.color.fg,
            2 => glyph.color.shadow,
            _ => glyph.color.bg,
        } & 0xF; // the window buffer is 4bpp
        // GLYPH_COPY_4BPP tests the *mapped palette index*, not the
        // source level. In particular bgColor=0 preserves the window's
        // checkerboard beneath a glyph; it must not punch through the BG.
        if index != 0 {
            return Some(palette[bank + usize::from(index)]);
        }
    }

    // The fill: FillWindowPixelBuffer's current value.
    let index = window
        .fills
        .iter()
        .rev()
        .find_map(|&(x, y, w, h, color)| {
            (wx >= usize::from(x)
                && wx < usize::from(x) + usize::from(w)
                && content_y >= usize::from(y)
                && content_y < usize::from(y) + usize::from(h))
            .then_some(color)
        })
        .unwrap_or(window.fill)
        & 0xF;
    (index != 0).then(|| palette[bank + usize::from(index)])
}

/// Samples one 4bpp tile of the owning layer's char block at
/// in-tile pixel `(px, py)` — the frame border and the arrow, whose
/// tilemap entries name palette bank `bank`.
fn sample_block_tile<S: AssetSource + ?Sized>(
    store: &S,
    palette: &[[u8; 4]],
    engine: &EngineFrame,
    window: &Window,
    tile: u32,
    bank: u8,
    px: usize,
    py: usize,
) -> Option<[u8; 4]> {
    let layer = &engine.bgs[usize::from(window.bg) & 3];
    let block = &engine.char_blocks[layer_index_char(layer)];
    let (tiles, local) = resolve_tile(store, block, tile as usize)?;
    if local >= usize::try_from(tiles.tile_count()).expect("u32 fits usize") {
        return None;
    }
    let value = tiles.pixels()[local * 64 + py * 8 + px];
    if value == 0 {
        return None; // color 0 is transparent
    }
    palette
        .get(usize::from(bank) * 16 + usize::from(value))
        .copied()
}

/// The alpha blend itself: `(first·EVA + second·EBV + 8) >> 4` per
/// channel, clamped — the hardware's weights over 16 with the
/// half-step rounding, both matching melonDS's software path exactly
/// (`GPU2D.cpp`'s `BLDALPHA` write clamps EVA/EBV to 16 — so
/// `G2_SetBlendAlpha(…, 31, 0)` is the *identity* "first target
/// whole", not a brightening — and `GPU2D_Soft.h`'s `ColorBlend4`
/// rounds with `+ 8`).
fn alpha_blend(first: [u8; 4], second: [u8; 4], eva: u8, ebv: u8) -> [u8; 4] {
    let eva = u32::from(eva.min(16));
    let ebv = u32::from(ebv.min(16));
    let mix = |a: u8, b: u8| {
        let v = (u32::from(a) * eva + u32::from(b) * ebv + 8) >> 4;
        u8::try_from(v.min(0xFF)).expect("clamped to u8")
    };
    [
        mix(first[0], second[0]),
        mix(first[1], second[1]),
        mix(first[2], second[2]),
        255,
    ]
}

/// The backdrop as RGBA8. The frame stores the raw `GX_RGB` BGR555
/// value (r bits 0–4, g 5–9, b 10–14); the expansion is the SDK's
/// exact `v << 3 | v >> 2`.
pub(crate) fn backdrop_rgba(color: u16) -> [u8; 4] {
    let expand = |v: u16| (v << 3 | v >> 2) as u8;
    [
        expand(color & 0x1F),
        expand(color >> 5 & 0x1F),
        expand(color >> 10 & 0x1F),
        255,
    ]
}

/// Applies the engine's master brightness to the whole screen, last.
fn apply_brightness(engine: &EngineFrame, screen: &mut ScreenBuffer) {
    let value = u32::from(engine.brightness.value);
    let adjust = |c: u8| match engine.brightness.mode {
        BrightnessMode::Disabled => c,
        // Up: toward white; down: toward black; both by value/16ths.
        BrightnessMode::Up => {
            let v = u32::from(c) + (((0xFF - u32::from(c)) * value) >> 4);
            u8::try_from(v.min(0xFF)).expect("clamped to u8")
        }
        BrightnessMode::Down => {
            // value 16 reaches black; larger weights saturate there.
            let sub = (u32::from(c) * value) >> 4;
            u8::try_from(u32::from(c).saturating_sub(sub)).expect("saturating keeps it in u8")
        }
    };
    for pixel in screen.pixels.iter_mut() {
        *pixel = [adjust(pixel[0]), adjust(pixel[1]), adjust(pixel[2]), 255];
    }
}

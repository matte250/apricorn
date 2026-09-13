//! The logical frame model — the 2D hardware state as pure data.
//!
//! One [`LogicalFrame`] is the complete observable video state at one
//! tick: both engines' BG layer configuration, their palette RAM
//! (composed from [`PaletteLoad`]s at raster time), the message
//! [`Window`]s printing into their layers, the blend and brightness
//! units, the backdrop colors, and which engine drives which LCD. No
//! pixel data lives here — layers and windows *reference* loaded
//! assets through [`AssetId`] handles that the asset store (see
//! `apicorn_core::assets`) resolves at raster time, so a frame is
//! plain data end to end: comparable with `==`, hashable, and
//! serializable for the harness without touching a ROM.
//!
//! The field geometry mirrors the hardware study (`docs/nds-2d.md`,
//! which cites the vendored NitroSDK headers and pret usage per fact):
//! engines A (MAIN, `0x04000000`) and B (SUB, `0x04001000`), four text
//! BG layers each under `BGxCNT`, one blend unit (`BLDCNT`/`BLDALPHA`)
//! and one master-brightness unit per engine.
//!
//! Since the message-window model arrived (Phase 4), the frame is no
//! longer `Copy` — windows and palette loads are `Vec`s — but stays
//! `Clone`/`Eq`/`Default`, the whole comparison contract.

/// A handle to one loaded asset in the engine's asset store.
///
/// Opaque by design: the store assigns these when a layer's tiles,
/// screen, or palette is loaded (Phase 3 step 4), and the rasterizer
/// (Phase 3 step 5) is the only consumer that resolves them. A frame
/// carries the handle and not the bytes, which keeps [`LogicalFrame`]
/// pure data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AssetId(u32);

impl AssetId {
    /// The first handle the store hands out.
    pub const FIRST: Self = Self(0);

    /// The next handle in sequence (the store's assignment order).
    #[must_use]
    pub fn next(self) -> Self {
        Self(self.0 + 1)
    }

    /// The raw store index.
    #[must_use]
    pub fn index(self) -> usize {
        self.0 as usize
    }

    /// The handle at `index` — the inverse of [`AssetId::index`], used
    /// by the store to hand out sequential handles in load order.
    #[must_use]
    pub fn from_index(index: usize) -> Self {
        Self(index as u32)
    }
}

/// Which engine drives which LCD — `GX_SetDispSelect` (`reg_GX_POWCNT`
/// bit 15), the register the game's `screensFlipped` idiom writes.
///
/// The rasterizer is display-agnostic: it renders engine A and engine
/// B; the presenter (or dump tool) maps them to LCDs using this.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DisplaySelect {
    /// `GX_DISP_SELECT_MAIN_SUB` (1): engine A on top, engine B on
    /// the touch LCD. The console's resting state.
    #[default]
    MainOnTop,
    /// `GX_DISP_SELECT_SUB_MAIN` (0): engine B on top, engine A on the
    /// touch LCD — HeartGold's title and intro state (`GfGfx_SwapDisplay`
    /// with `screensFlipped`).
    SubOnTop,
}

/// A text BG layer's color mode — `BGxCNT` bit 7
/// (`GX_BG_COLORMODE_16` / `GX_BG_COLORMODE_256`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ColorMode {
    /// 4bpp: 16-color banks, the screen entry's bits 12–15 select one.
    #[default]
    Bpp4,
    /// 8bpp: the full 256-color engine palette, entry palette bits ignored.
    Bpp8,
}

/// A text BG layer's map size — `BGxCNT` bits 13–14
/// (`GX_BG_SCRSIZE_TEXT_*`).
///
/// Sizes above 256×256 store the map as 2 KiB 32×32 blocks: 512-wide
/// sizes read their left/right block pairs per row; 512-tall sizes
/// stack top/bottom block pairs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ScreenSize {
    /// 256×256 (`GX_BG_SCRSIZE_TEXT_256x256`).
    #[default]
    W256xH256,
    /// 512×256 (`GX_BG_SCRSIZE_TEXT_512x256`).
    W512xH256,
    /// 256×512 (`GX_BG_SCRSIZE_TEXT_256x512`).
    W256xH512,
    /// 512×512 (`GX_BG_SCRSIZE_TEXT_512x512`).
    W512xH512,
}

impl ScreenSize {
    /// The layer's map width in 8×8 tiles (32 or 64).
    #[must_use]
    pub fn tiles_wide(self) -> u16 {
        match self {
            Self::W256xH256 | Self::W256xH512 => 32,
            Self::W512xH256 | Self::W512xH512 => 64,
        }
    }

    /// The layer's map height in 8×8 tiles (32 or 64).
    #[must_use]
    pub fn tiles_tall(self) -> u16 {
        match self {
            Self::W256xH256 | Self::W512xH256 => 32,
            Self::W256xH512 | Self::W512xH512 => 64,
        }
    }
}

/// One text BG layer — the parts of `BGxCNT` plus the scroll registers
/// (`BGxHOFS`/`BGxVOFS`) that the rasterizer needs.
///
/// `char_base` indexes [`EngineFrame::char_blocks`], not VRAM: the
/// model names slots, so layers that share a block (the copyright
/// beat's SUB BG0 and BG1 share one charbase) see the same tile data
/// without emulating banking.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BgLayer {
    /// Whether the layer is shown (`G2_BG0_ON`/…/`G2S_BG3_ON`).
    pub enabled: bool,
    /// Which char-block slot (0–7) holds this layer's 8×8 tiles.
    pub char_base: u8,
    /// The layer's screen (map) asset.
    pub screen: Option<AssetId>,
    /// 4bpp or 8bpp (`BGxCNT` bit 7).
    pub color_mode: ColorMode,
    /// Map size (`BGxCNT` bits 13–14).
    pub size: ScreenSize,
    /// Horizontal scroll (raw `BGxHOFS`, 0–511).
    pub scroll_x: u16,
    /// Vertical scroll (raw `BGxVOFS`, 0–511).
    pub scroll_y: u16,
    /// Draw priority 0–3 (`BGxCNT` bits 0–1): lower draws on top;
    /// ties go to the lower-numbered BG.
    pub priority: u8,
    /// Screen-space rectangle where the hardware window hides this BG,
    /// `(left, top, right, bottom)` with exclusive right/bottom edges.
    pub hidden_rect: Option<(u16, u16, u16, u16)>,
}

impl Default for BgLayer {
    fn default() -> Self {
        Self {
            enabled: false,
            char_base: 0,
            screen: None,
            color_mode: ColorMode::Bpp4,
            size: ScreenSize::W256xH256,
            scroll_x: 0,
            scroll_y: 0,
            priority: 0,
            hidden_rect: None,
        }
    }
}

/// One tile-set asset placed in a char block — the model's stand-in
/// for the `LoadCharData`-at-an-offset idiom. Every NCGR a layer's
/// tilemap can name lands in the *same* char block at its own tile
/// offset (the Oak speech's MAIN BG0 holds the intro art at tile 0,
/// the frame graphics at 0x3D9, the frame again at 0x3E2), so a
/// block is a list of placements and a tile index resolves against
/// whichever placement covers it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TilePlacement {
    /// The placed Tiles asset.
    pub asset: AssetId,
    /// The placement's tile offset within the char block.
    pub tile: u16,
}

/// One palette-file load into the engine's BG palette RAM — the
/// model of `GfGfxLoader_GXLoadPal` at an offset: a 16-color frame
/// palette into bank 4, the font palettes at bank 12, a whole 0x200
/// byte file over slots 0–15.
///
/// Loads are composed in order at raster time ([`EngineFrame`]):
/// later loads overwrite earlier ones, exactly as palette RAM does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PaletteLoad {
    /// The loaded palette asset (PMCP applied at placement).
    pub asset: AssetId,
    /// The RAM offset in colors — the load's byte offset over 2.
    pub offset: u16,
    /// How many colors land in RAM (the load's byte size over 2;
    /// a whole-file load carries the file's color count).
    pub colors: u16,
}

/// A text color triple — pret's `MAKE_TEXT_COLOR(fg, shadow, bg)`:
/// palette *indices within the printing window's bank*, applied to
/// glyph levels 1/2/3 at raster time (level 0 is transparent).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextColor {
    /// The foreground index (level 1).
    pub fg: u8,
    /// The shadow index (level 2).
    pub shadow: u8,
    /// The background index (level 3) — also the window's fill.
    pub bg: u8,
}

impl TextColor {
    /// `MAKE_TEXT_COLOR(fg, shadow, bg)` — the fields in pret's
    /// argument order (`fgColor = color >> 16`, `shadow = >> 8`,
    /// `bg = >> 0`).
    #[must_use]
    pub const fn new(fg: u8, shadow: u8, bg: u8) -> Self {
        Self { fg, shadow, bg }
    }
}

/// One glyph placed in a window — the printer's unit of progress.
///
/// `x`/`y` are the window-relative pixels where the glyph's top-left
/// landed (the `currentX`/`currentY` of the print step), in the
/// window's *unscrolled* space: the [`Window::scroll`] translation
/// applies to all content at once, the way `ScrollWindow` shifts the
/// whole pixel buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowGlyph {
    /// The font asset the glyph renders from.
    pub font: AssetId,
    /// The character code unit printed (1-based glyph id; the font's
    /// own fallback applies past its table).
    pub glyph: u16,
    /// The window-relative X at print time.
    pub x: u16,
    /// The window-relative Y at print time.
    pub y: u16,
    /// The color triple active at print time.
    pub color: TextColor,
    /// The `{SIZE 200}` row-doubling table (pret's `glyphTable`
    /// 0xFFFC): every other source row copies one pixel down —
    /// the half-height large-print effect.
    pub double_rows: bool,
}

/// A window's border, drawn from graphics in its own char block.
/// Menus use `DrawFrameAndWindow1`'s nine tiles; dialogue uses
/// `DrawFrameAndWindow2`'s wider eighteen-tile layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowFrame {
    /// The char-block tile offset the frame NCGR loaded at.
    pub base_tile: u16,
    /// The palette bank of the border's tilemap entries (the frame
    /// NCLR loads 16 colors into this bank).
    pub palette: u8,
    /// True for `DrawFrameAndWindow2`: an 18-tile dialogue border,
    /// two tiles left and three tiles right of the window interior.
    pub dialogue: bool,
}

/// The wait-for-input down arrow — `TextPrinter_DrawDownArrow`'s
/// 2×2-tile animation, drawn into the tilemap one tile right of the
/// window's bottom-right corner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowArrow {
    /// The dialogue frame's base tile (`sDownArrowBaseTile`). Three
    /// composed arrow poses live at `+18..+29`; clearing restores
    /// border tiles `+10/+11`.
    pub base_tile: u16,
    /// The animation position — one step per 8 held frames, walking
    /// `sDownArrowTileOffsets` = {0, 1, 2, 1}.
    pub index: u8,
}

/// The arrow animation's tile-offset cycle — pret's
/// `sDownArrowTileOffsets` (`src/render_text.c`).
pub const ARROW_TILE_OFFSETS: [u8; 4] = [0, 1, 2, 1];

/// The {YESNO} focus indicator — pret `text.c`'s
/// `RenderScreenFocusIndicatorTile`: a 24×32 blit of frame `index`
/// out of the focus NCGR (`NARC_graphic_font` member 6, four
/// 384-byte frames of twelve 8×8 4bpp tiles), landed at
/// `((width - 3) · 8, 0)` of the window's pixel buffer with color 0
/// transparent.
///
/// The blit is *into the buffer*: it erases under later glyphs and
/// scrolls with the window. The model carries the print-time facts
/// — the frame index and the cumulative [`Window::scroll`] when the
/// indicator landed, so a later [`Window::scroll`] shifts it exactly
/// as `ScrollWindow` would — and the raster re-derives the pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowFocus {
    /// The focus NCGR (a 48-tile Tiles asset: frames of 12 tiles).
    pub asset: AssetId,
    /// The frame blitted — pret's `fieldNum`, `0..4`.
    pub index: u8,
    /// The window's cumulative scroll at blit time (the buffer row 0
    /// the indicator landed on, in content coordinates).
    pub scroll: u16,
}

/// A message window — pret's `Window` (`bg_window.c`) as content
/// rather than a VRAM pixel buffer.
///
/// The original allocates `width × height` tiles of char block at
/// `base_tile`, fills them, blits glyphs in, and points the layer's
/// tilemap at them; the rasterizer here re-derives exactly that from
/// the fill, the placed [`WindowGlyph`]s, and the scroll — so the
/// model carries the printing state, never pixels. Geometry is in
/// tiles on the owning BG's map (windows scroll with their layer,
/// because the original's tilemap entries do).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Window {
    /// The owning BG layer index (0–3).
    pub bg: u8,
    /// The rect's left edge, in tiles.
    pub left: u8,
    /// The rect's top edge, in tiles.
    pub top: u8,
    /// The rect's width, in tiles.
    pub width: u8,
    /// The rect's height, in tiles.
    pub height: u8,
    /// The palette bank of the window's tilemap entries — the bank
    /// its fill and glyph indices decode against.
    pub palette: u8,
    /// The char-block tile offset of the window's pixel-buffer tiles
    /// (provenance; the content model needs no asset there).
    pub base_tile: u16,
    /// The fill color index — `FillWindowPixelBuffer`'s current
    /// value, the window's background.
    pub fill: u8,
    /// Rectangle fills applied over the background and before glyphs, in
    /// window content pixels: `(x, y, width, height, palette_index)`.
    pub fills: Vec<(u16, u16, u16, u16, u8)>,
    /// The glyphs printed so far, in print order (later glyphs draw
    /// over earlier ones, as blits do).
    pub glyphs: Vec<WindowGlyph>,
    /// The cumulative pixel scroll (`ScrollWindow`'s shifting, in
    /// the original applied to the buffer; here a translation).
    pub scroll: u16,
    /// The menu or dialogue border around the rect, if drawn.
    pub frame: Option<WindowFrame>,
    /// The wait indicator at the rect's bottom-right corner, if
    /// shown.
    pub arrow: Option<WindowArrow>,
    /// The {YESNO} focus indicator blit into the buffer, if the
    /// printed message carried a `{YESNO 0}` block.
    pub focus: Option<WindowFocus>,
}

impl Window {
    /// The window's interior rect in map pixels: `(x, y, w, h)`.
    #[must_use]
    pub fn rect_px(&self) -> (u16, u16, u16, u16) {
        (
            u16::from(self.left) * 8,
            u16::from(self.top) * 8,
            u16::from(self.width) * 8,
            u16::from(self.height) * 8,
        )
    }
}

impl Default for Window {
    fn default() -> Self {
        Self {
            bg: 0,
            left: 0,
            top: 0,
            width: 0,
            height: 0,
            palette: 0,
            base_tile: 0,
            fill: 0,
            fills: Vec::new(),
            glyphs: Vec::new(),
            scroll: 0,
            frame: None,
            arrow: None,
            focus: None,
        }
    }
}

/// The blend plane mask bits — `BLDCNT` bits 0–5 (first target) and
/// 8–13 (second target); the SDK's `GXBlendPlaneMask` values.
pub mod plane {
    /// Engine BG0.
    pub const BG0: u8 = 0x01;
    /// Engine BG1.
    pub const BG1: u8 = 0x02;
    /// Engine BG2.
    pub const BG2: u8 = 0x04;
    /// Engine BG3.
    pub const BG3: u8 = 0x08;
    /// The OBJ (sprite) plane.
    pub const OBJ: u8 = 0x10;
    /// The backdrop.
    pub const BD: u8 = 0x20;
}

/// The blend effect in progress — `BLDCNT` bits 6–7.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BlendEffect {
    /// No blending.
    #[default]
    None,
    /// Alpha blend: first target weighted EVA, second EBV.
    Alpha,
    /// Brightness-down (bit 7): the first-target planes darken by
    /// `Blend::evy` — the blend unit's own fade, `G2_SetBlendBrightness`'s
    /// negative range, distinct from [`MasterBrightness`].
    BrightnessDown,
    /// Brightness-up (bit 6): the first-target planes lighten by
    /// `Blend::evy` — `G2_SetBlendBrightness`'s positive range.
    BrightnessUp,
}

/// One engine's blend unit — `BLDCNT`/`BLDALPHA`/`BLDY`.
///
/// The alpha effect carries its weights on [`Blend::eva`]/[`Blend::ebv`];
/// the brightness effects carry the `BLDY` weight on [`Blend::evy`]
/// (the sign that picked the effect is the effect itself —
/// `GXx_SetBlendBrightness_` writes Down for a negative value, Up for a
/// positive one). The rasterizer applies this to the displayed target
/// plane before master brightness, including Oak's OBJ flash.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Blend {
    /// The first-target plane mask (`plane::*` bits).
    pub plane1: u8,
    /// The effect (none, alpha blend, or a brightness fade).
    pub effect: BlendEffect,
    /// The second-target plane mask (`plane::*` bits).
    pub plane2: u8,
    /// First-target weight, `BLDALPHA` bits 0–4 (0–31).
    pub eva: u8,
    /// Second-target weight, `BLDALPHA` bits 8–12 (0–31).
    pub ebv: u8,
    /// The brightness effects' weight, `BLDY` bits 0–4 (0–16 in
    /// practice).
    pub evy: u8,
}

/// One edit made to a layer's tilemap buffer after its screen asset
/// loaded — the model of pret's `BgTilemapRectChangePalette` and
/// `FillBgTilemapRect` (`bg_window.c`), the direct map rewrites the
/// scenes run between screen loads.
///
/// Edits are scene state, applied at raster time *after* the layer's
/// screen asset, in push order — a later edit wins, exactly as the
/// tilemap buffer's last write does. The scene owns the lifetime: a
/// screen load or a tilemap clear for a layer drops that layer's
/// earlier edits (the load overwrote the buffer; the clear zeroed it).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TilemapEdit {
    /// `BgTilemapRectChangePalette` — every entry in the rect decodes
    /// against palette bank `bank` instead of its own.
    Palette {
        /// The edited layer (0–3).
        bg: u8,
        /// The palette bank the rect's entries take.
        bank: u8,
        /// The rect's left edge, in tiles.
        left: u8,
        /// The rect's top edge, in tiles.
        top: u8,
        /// The rect's width, in tiles.
        width: u8,
        /// The rect's height, in tiles.
        height: u8,
    },
    /// `FillBgTilemapRect` — the rect's entries become `tile` at
    /// palette `palette`, no flips.
    Fill {
        /// The edited layer (0–3).
        bg: u8,
        /// The tile the rect fills with.
        tile: u16,
        /// The rect's left edge, in tiles.
        left: u8,
        /// The rect's top edge, in tiles.
        top: u8,
        /// The rect's width, in tiles.
        width: u8,
        /// The rect's height, in tiles.
        height: u8,
        /// The palette bank of the filled entries.
        palette: u8,
    },
}

/// One engine's master-brightness unit — `MASTER_BRIGHT`
/// (`0x0400006C` engine A, `0x0400106C` engine B).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MasterBrightness {
    /// The direction of the effect (`E_MOD` bits 14–15): disabled,
    /// toward white (up), or toward black (down).
    pub mode: BrightnessMode,
    /// The weight, `value` bits 0–4 — 0–16 in practice (31 max).
    pub value: u8,
}

/// The master-brightness mode — `E_MOD` bits 14–15.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BrightnessMode {
    /// Mode 0: no effect (`SetMasterBrightnessNeutral`).
    #[default]
    Disabled,
    /// Mode 1: fade toward white.
    Up,
    /// Mode 2: fade toward black.
    Down,
}

/// A sprite assembled from a ROM cell and animation bank.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sprite {
    /// Character graphics.
    pub tiles: AssetId,
    /// OBJ palette, independent of BG palette RAM.
    pub palette: AssetId,
    /// Cell geometry and OAM attributes.
    pub cells: AssetId,
    /// Animation sequences.
    pub animation: AssetId,
    /// Active sequence index.
    pub sequence: usize,
    /// Ticks since the sequence was selected.
    pub elapsed: u32,
    /// Screen-space origin.
    pub x: i16,
    /// Screen-space origin.
    pub y: i16,
    /// Hardware OBJ priority relative to BG layers (0 is foremost).
    pub priority: u8,
    /// Base palette bank within the sprite's loaded palette. Each OAM
    /// entry's bank is added to this placement offset.
    pub palette_bank: u8,
}

/// One map-object billboard to draw this tick — the `mmodel` quad of
/// `a/0/8/1` with one texel rectangle of its NSBTX shown
/// (`docs/gfx.md`, "The billboard shear and map objects"). The field
/// system builds one per visible object every tick; the rasterizer
/// places the quad camera-facing at `world_pos` (the object's
/// position vector — its feet) and `size_px` world units wide and
/// tall, which the field presets scale ≈1:1 to pixels.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectView {
    /// The decoded frame strip (or single image): RGBA8, shared so a
    /// frame clone never copies pixels.
    pub texture: std::sync::Arc<crate::field::model::Texture>,
    /// The texel rectangle `(u, v, width, height)` shown — the walk
    /// frame within the strip.
    pub rect: (u16, u16, u16, u16),
    /// The anchor in fx32 world coordinates: the bottom centre of the
    /// quad.
    pub world_pos: [i32; 3],
    /// The quad's width and height in world units (32 × 32 for the
    /// standard `mmdl_m32x32` class).
    pub size_px: (u16, u16),
    /// Draw the rectangle mirrored left-to-right — the classes that
    /// store one side view and flip it for the other facing.
    pub mirrored: bool,
}

/// The field (3D) layer as one tick sees it — the plain-data view the
/// field system publishes and the rasterizer draws as engine A's BG0
/// (`docs/field-system.md`): the loaded map, the camera preset and its
/// target, and the map-object billboards.
///
/// The scene is shared by `Arc` (a map load is the expensive part and
/// happens only at warps); everything per-tick — the camera target
/// (`FieldCamera`'s tracked position vector) and the objects' frames
/// and positions — is copied into the frame, so a frame compares with
/// `==` and replays without the system that produced it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldFrame {
    /// The loaded map: cells, props, terrain, events.
    pub scene: std::sync::Arc<crate::field::FieldScene>,
    /// The camera preset in force (`FieldCamera_Create`'s row; scripts
    /// may replace it later).
    pub camera: crate::field::ov01::CameraPreset,
    /// The camera's look-at target in fx32 world units — the player's
    /// position vector (`Camera_SetFixedTarget`), before the preset's
    /// look-at offset.
    pub camera_target: [i32; 3],
    /// The area-light registers active for this tick. Static preview
    /// frames omit them; the live field always supplies the ROM table.
    pub lighting: Option<crate::field::lighting::ModelLighting>,
    /// The visible map objects, in draw order.
    pub objects: Vec<ObjectView>,
}

impl FieldFrame {
    /// The static view of a loaded scene: the header's camera preset
    /// targeting the player's tile, and the scene's south-facing player
    /// image as the one object standing on that tile — what the
    /// Phase 4 bedroom landing showed, placed as the live system places
    /// a standing player: the tile centre lifted to the ground height
    /// the cells' BDHC plates give (`field::height`, 0 where none
    /// covers the tile) for the camera target, plus
    /// [`SPRITE_OFFSET`](crate::field::system::SPRITE_OFFSET) for the
    /// billboard's anchor.
    #[must_use]
    pub fn static_scene(scene: std::sync::Arc<crate::field::FieldScene>) -> Self {
        use crate::field::height::{HeightMode, scene_height};
        use crate::field::map_object::VecFx32;
        use crate::field::system::SPRITE_OFFSET;
        let [x, z] = scene.position;
        let tile = VecFx32::from_tile(x, 0, z);
        let y = scene_height(&scene.cells, HeightMode::Nearest, tile.x, 0, tile.z).unwrap_or(0);
        let target = [tile.x, y, tile.z];
        let world_pos = [
            tile.x + SPRITE_OFFSET.x,
            y + SPRITE_OFFSET.y,
            tile.z + SPRITE_OFFSET.z,
        ];
        let player = std::sync::Arc::new(scene.player.clone());
        let size = (
            u16::try_from(player.width).unwrap_or(u16::MAX),
            u16::try_from(player.height).unwrap_or(u16::MAX),
        );
        let camera = scene.camera;
        Self {
            scene,
            camera,
            camera_target: target,
            lighting: None,
            objects: vec![ObjectView {
                texture: player,
                rect: (0, 0, size.0, size.1),
                world_pos,
                size_px: size,
                mirrored: false,
            }],
        }
    }
}

/// One 2D engine's state — four text BG layers over the blend and
/// brightness units, the named char-block slots, the composed BG
/// palette RAM, and the message windows printing into the layers.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct EngineFrame {
    /// The 3D field layer, when this engine displays a field map.
    pub field: Option<FieldFrame>,
    /// The engine's four BG layers, BG0 through BG3. Engine A's BG0
    /// doubles as the 3D core's framebuffer when bound to a model
    /// (`GX_BG0_AS_3D`) — Phase 3 renders it absent (transparent),
    /// which a disabled layer already expresses.
    pub bgs: [BgLayer; 4],
    /// The char-block slots (indices 0–7) that `char_base` names —
    /// each a list of [`TilePlacement`]s at their tile offsets.
    /// Layers share a slot by naming the same index.
    pub char_blocks: [Vec<TilePlacement>; 8],
    /// The engine's alpha-blend unit.
    pub blend: Blend,
    /// The engine's master-brightness unit.
    pub brightness: MasterBrightness,
    /// The engine's BG palette RAM as composed at raster time: 256
    /// colors built from [`PaletteLoad`]s in order — the one palette
    /// every 4bpp bank and 8bpp pixel of this engine decodes against,
    /// exactly the hardware's single RAM.
    pub palette_loads: Vec<PaletteLoad>,
    /// Live BG palette words (color index, BGR555), applied after asset
    /// loads. Scenes replace entries in place and clear affected entries
    /// when reloading that palette range.
    pub palette_overrides: Vec<(u16, u16)>,
    /// Live OBJ palette words (absolute color index, BGR555).
    pub obj_palette_overrides: Vec<(u16, u16)>,
    /// The message windows on this engine's layers.
    pub windows: Vec<Window>,
    /// Visible sprites in OAM order (earlier wins a sprite overlap).
    pub sprites: Vec<Sprite>,
    /// The tilemap-buffer rewrites made since the layers' screen
    /// assets loaded, applied after them at raster time in push order
    /// (a later edit wins). A screen load or tilemap clear for a layer
    /// drops that layer's earlier edits — the scene owns the lifetime.
    pub tilemap_edits: Vec<TilemapEdit>,
    /// The backdrop ("mask") color — raw BGR555 (`GX_RGB` layout:
    /// r bits 0–4, g 5–9, b 10–14); the color shown wherever no layer
    /// covers the pixel.
    pub backdrop: u16,
}

/// The complete observable video state at one tick.
///
/// Engine A (MAIN) and engine B (SUB) each render a 256×192 screen;
/// [`LogicalFrame::display`] says which LCD each drives. The frame
/// after a tick is what the rasterizer turns into pixels and what
/// frame-indexed tests (and, later, the harness) compare.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LogicalFrame {
    /// Engine A (MAIN, register base `0x04000000`).
    pub main: EngineFrame,
    /// Engine B (SUB, register base `0x04001000`).
    pub sub: EngineFrame,
    /// The engine→LCD mapping (`GX_SetDispSelect`).
    pub display: DisplaySelect,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_frame_is_both_engines_dark() {
        // The power-on shape: everything off, black backdrop, MAIN on
        // top — the state every app starts its layer setup from.
        let frame = LogicalFrame::default();
        assert_eq!(frame.display, DisplaySelect::MainOnTop);
        for engine in [&frame.main, &frame.sub] {
            assert_eq!(engine.backdrop, 0, "black backdrop");
            assert_eq!(engine.blend, Blend::default());
            assert_eq!(engine.brightness, MasterBrightness::default());
            assert!(engine.bgs.iter().all(|bg| !bg.enabled));
            assert!(engine.char_blocks.iter().all(|block| block.is_empty()));
            assert!(engine.palette_loads.is_empty());
            assert!(engine.windows.is_empty());
        }
    }

    #[test]
    fn screen_sizes_cover_the_text_layouts() {
        let cases = [
            (ScreenSize::W256xH256, 32, 32),
            (ScreenSize::W512xH256, 64, 32),
            (ScreenSize::W256xH512, 32, 64),
            (ScreenSize::W512xH512, 64, 64),
        ];
        for (size, wide, tall) in cases {
            assert_eq!(size.tiles_wide(), wide);
            assert_eq!(size.tiles_tall(), tall);
        }
    }

    #[test]
    fn asset_ids_sequence_like_the_store_assigns() {
        // The store hands out FIRST then next() in load order; frames
        // compare and hash by handle value.
        let a = AssetId::FIRST;
        let b = a.next().next();
        assert_eq!(b.index(), 2);
        assert_ne!(a, b);
    }

    #[test]
    fn layers_sharing_a_char_base_share_the_slot() {
        // The copyright beat's SUB BG0 and BG1 read the same tiles:
        // same char_base, one placement, two screen references.
        let mut sub = EngineFrame::default();
        sub.char_blocks[4].push(TilePlacement {
            asset: AssetId::FIRST,
            tile: 0,
        });
        sub.bgs[0].char_base = 4;
        sub.bgs[0].screen = Some(AssetId::FIRST.next());
        sub.bgs[1].char_base = 4;
        sub.bgs[1].screen = Some(AssetId::FIRST.next().next());
        assert_eq!(sub.bgs[0].char_base, sub.bgs[1].char_base);
        assert_eq!(
            sub.char_blocks[4],
            sub.char_blocks[usize::from(sub.bgs[1].char_base)]
        );
    }

    #[test]
    fn tile_placements_cover_offsets_within_one_block() {
        // The Oak speech's MAIN BG0: intro art at tile 0, a frame at
        // 0x3D9, another at 0x3E2 — one block, three placements, each
        // tile index resolving to the placement that covers it.
        let mut engine = EngineFrame::default();
        for (asset, tile) in [(AssetId::FIRST, 0u16), (AssetId::FIRST.next(), 0x3D9)] {
            engine.char_blocks[6].push(TilePlacement { asset, tile });
        }
        assert_eq!(engine.char_blocks[6][0].tile, 0);
        assert_eq!(engine.char_blocks[6][1].tile, 0x3D9);
    }

    #[test]
    fn windows_are_full_printing_state() {
        // A dialog window mid-print: filled, two glyphs down, framed,
        // waiting with the arrow.
        let window = Window {
            bg: 0,
            left: 2,
            top: 19,
            width: 27,
            height: 4,
            palette: 12,
            base_tile: 0x36D,
            fill: 0xF,
            fills: Vec::new(),
            glyphs: vec![
                WindowGlyph {
                    font: AssetId::FIRST,
                    glyph: 300,
                    x: 0,
                    y: 0,
                    color: TextColor::new(1, 2, 0xF),
                    double_rows: false,
                },
                WindowGlyph {
                    font: AssetId::FIRST,
                    glyph: 301,
                    x: 8,
                    y: 0,
                    color: TextColor::new(1, 2, 0xF),
                    double_rows: false,
                },
            ],
            scroll: 0,
            frame: Some(WindowFrame {
                base_tile: 0x3E2,
                palette: 4,
                dialogue: false,
            }),
            arrow: Some(WindowArrow {
                base_tile: 0,
                index: 2,
            }),
            focus: Some(WindowFocus {
                asset: AssetId::FIRST.next().next(),
                index: 1,
                scroll: 0,
            }),
        };
        assert_eq!(window.rect_px(), (16, 152, 216, 32));
        assert_eq!(window.glyphs.len(), 2);
        assert_eq!(window.glyphs[1].x, 8);
        // The arrow cycles pret's offsets.
        assert_eq!(ARROW_TILE_OFFSETS, [0, 1, 2, 1]);
        // The focus indicator: the naming screen's "Your name?" blit,
        // frame 1, landed before any scroll.
        let focus = window.focus.expect("the fixture sets a focus");
        assert_eq!(focus.index, 1);
        assert_eq!(focus.scroll, 0);
        // A later scroll shifts the blit in lockstep — the content row
        // it occupies grows by the difference.
        let scrolled = WindowFocus {
            scroll: 16,
            ..focus
        };
        assert_eq!(
            scrolled.scroll - focus.scroll,
            16,
            "the scroll delta is the blit's shift"
        );
    }

    #[test]
    fn plane_mask_bits_match_the_sdk() {
        // GXBlendPlaneMask (docs/nds-2d.md): the six bits, in order.
        assert_eq!(plane::BG0, 0x01);
        assert_eq!(plane::BG1, 0x02);
        assert_eq!(plane::BG2, 0x04);
        assert_eq!(plane::BG3, 0x08);
        assert_eq!(plane::OBJ, 0x10);
        assert_eq!(plane::BD, 0x20);
    }
}

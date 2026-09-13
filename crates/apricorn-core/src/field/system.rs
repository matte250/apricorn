//! The live field system — pret's `FieldSystem` (`src/field_system.c`)
//! for the part of the overworld that exists so far: the player walking
//! a loaded map, the camera following, the player billboard animated
//! from the real map-object frames, the new-game fade-in, and the warp
//! tasks between maps (`docs/field-system.md`).
//!
//! Per-frame order is `NitroMain`'s (`src/main.c:95-130`) one loop
//! iteration — which spans **two** VBlanks in retail (the loop waits
//! for VBlank twice unless a frame ran long), so one [`FieldSystem::tick`]
//! is one retail game tick, not one VBlank — through
//! `FieldSystem_Main` (`src/field_system.c:180`):
//!
//! 1. `FieldSystem_Control` (`:203`): when movement is allowed (no field
//!    task running, the map up), `PlayerAvatar_UpdateMovement` →
//!    `FieldInput_Update` → `FieldInput_Process`
//!    (`src/field/field_control.c:193`) → `PlayerAvatar_MoveControl`;
//! 2. `FieldSystem_RunTaskFrame` (`src/task.c:49`): the field task
//!    stack — the new-game entry (`FieldTask_NewGame`,
//!    `src/field_warp_tasks.c:386`) and the map transitions
//!    (`sub_02055DBC`, `src/unk_02055BF0.c:135`);
//! 3. the field overlay's frame (`FieldMap_Main`, `src/field/fieldmap.c:237`
//!    → `ov01_021E6220`): the camera pushed at the player's position
//!    vector (`Camera_PushLookAtToNNSGlb`, `src/camera.c:157`) and the
//!    geometry and billboards submitted — the frame this tick displays;
//! 4. the SysTask queue: every map object's `sub_0205F12C`
//!    (`src/map_object.c:893`) — the movement step (`sub_0205FD30`) then
//!    the sprite update (`ov01_021F92A0` → `ov01_021F772C`,
//!    `asm/overlay_01_021F72DC.s:562`);
//! 5. after the VBlank, `HandleFadeUpdateFrame`'s brightness step.
//!
//! So the frame a tick returns shows the objects where the previous
//! tick's step left them and the brightness this tick's fade step
//! wrote — the retail display latency, kept so the oracle's screenshot
//! sequence lines up tick for tick (`docs/field-system.md`, "Measured on
//! the oracle").

use std::sync::Arc;

use super::avatar::{
    AvatarMoveState, EncounterSteps, MoveOutcome, PlayerAvatar, PlayerMoveState, PlayerSaveData,
    PlayerState,
};
use super::events::WarpEvent;
use super::height::{HeightMode, scene_height};
use super::input::{FieldInput, FieldInputContext};
use super::lighting::{AreaLightArchive, AreaLightManager, ModelLighting, archive_for_light_type};
use super::map_header::MapHeaders;
use super::map_object::{
    ATTR_NONE, Collision, Direction, FX32_ONE, MapObject, MovementCmd, TILE_FX32, VecFx32, family,
    flag,
};
use super::model::{Texture, decode_texture};
use super::ov01::{self, CameraPreset, Ov01, SpriteModelTable};
use super::terrain::TerrainAttributes;
use super::{FieldScene, LandCell};
use crate::assets::{AssetStore, AssetsError, narc_member};
use crate::formats::Btx;
use crate::frame::{BrightnessMode, FieldFrame, LogicalFrame, MasterBrightness, ObjectView};
use crate::input::{Input, Keys};
use crate::nds::NdsError;

// ---------------------------------------------------------------------------
// Metatile behaviours the warp rules read (include/constants/metatile_behavior.h)
// ---------------------------------------------------------------------------

/// `TILE_BEHAVIOR_LADDER_NORTH`.
pub const BEHAVIOR_LADDER_NORTH: u8 = 60;
/// `TILE_BEHAVIOR_LADDER_SOUTH`.
pub const BEHAVIOR_LADDER_SOUTH: u8 = 61;
/// `TILE_BEHAVIOR_LADDER_DOWN`.
pub const BEHAVIOR_LADDER_DOWN: u8 = 62;
/// `TILE_BEHAVIOR_WARP_STAIRS_EAST`.
pub const BEHAVIOR_WARP_STAIRS_EAST: u8 = 94;
/// `TILE_BEHAVIOR_WARP_STAIRS_WEST` — the bedroom's stairs tile (0x5F).
pub const BEHAVIOR_WARP_STAIRS_WEST: u8 = 95;
/// `TILE_BEHAVIOR_WARP_ENTRANCE_EAST`.
pub const BEHAVIOR_WARP_ENTRANCE_EAST: u8 = 98;
/// `TILE_BEHAVIOR_WARP_ENTRANCE_WEST`.
pub const BEHAVIOR_WARP_ENTRANCE_WEST: u8 = 99;
/// `TILE_BEHAVIOR_WARP_ENTRANCE_NORTH`.
pub const BEHAVIOR_WARP_ENTRANCE_NORTH: u8 = 100;
/// `TILE_BEHAVIOR_WARP_ENTRANCE_SOUTH` — the doormat inside a house.
pub const BEHAVIOR_WARP_ENTRANCE_SOUTH: u8 = 101;
/// `TILE_BEHAVIOR_WARP_PANEL`.
pub const BEHAVIOR_WARP_PANEL: u8 = 103;
/// `TILE_BEHAVIOR_DOOR` — a building's door tile, seen from outside.
pub const BEHAVIOR_DOOR: u8 = 105;
/// `TILE_BEHAVIOR_ESCALATOR_FLIP_FACE`.
pub const BEHAVIOR_ESCALATOR_FLIP_FACE: u8 = 106;
/// `TILE_BEHAVIOR_ESCALATOR`.
pub const BEHAVIOR_ESCALATOR: u8 = 107;
/// `TILE_BEHAVIOR_WARP_EAST`.
pub const BEHAVIOR_WARP_EAST: u8 = 108;
/// `TILE_BEHAVIOR_WARP_WEST`.
pub const BEHAVIOR_WARP_WEST: u8 = 109;
/// `TILE_BEHAVIOR_WARP_NORTH`.
pub const BEHAVIOR_WARP_NORTH: u8 = 110;
/// `TILE_BEHAVIOR_WARP_SOUTH`.
pub const BEHAVIOR_WARP_SOUTH: u8 = 111;

/// `WarpEvent.anchor == 0x100`: the destination is the save's dynamic
/// warp (`FieldSystem_MapConnection`, `src/field/field_control.c:915`).
pub const DYNAMIC_WARP_ANCHOR: u16 = 0x100;

// ---------------------------------------------------------------------------
// Location
// ---------------------------------------------------------------------------

/// pret's `Location` (`include/field_types_def.h:10`): where the player
/// is, or is going. `warp_id` is `-1` when `x`/`z` are the destination
/// themselves; otherwise the destination map's warp event `warp_id`
/// supplies them at map load (`sub_02052F94`, `src/field_warp_tasks.c:197`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Location {
    /// `mapId`.
    pub map_id: u16,
    /// `warpId`: an index into the map's warp events, or `-1`.
    pub warp_id: i32,
    /// `x`, in matrix-wide tiles.
    pub x: i32,
    /// `y` — pret's name for the tile z coordinate.
    pub z: i32,
    /// `direction`: the facing on arrival.
    pub direction: Direction,
}

impl Location {
    /// `sLocation_PlayerRoom` (`src/location_backup.c:9`): map 64, no
    /// warp, tile (6, 6), facing south — where a new game starts.
    pub const PLAYER_ROOM: Self = Self {
        map_id: 64,
        warp_id: -1,
        x: 6,
        z: 6,
        direction: Direction::South,
    };

    /// `InitLocation`.
    #[must_use]
    pub const fn new(map_id: u16, warp_id: i32, x: i32, z: i32, direction: Direction) -> Self {
        Self {
            map_id,
            warp_id,
            x,
            z,
            direction,
        }
    }
}

// ---------------------------------------------------------------------------
// The brightness fade (BeginNormalPaletteFade, FADE_BOTH_SCREENS, black)
// ---------------------------------------------------------------------------

/// Which way a black fade runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FadeDirection {
    /// `FADE_TYPE_BRIGHTNESS_IN`: from −16 (black) to 0.
    In,
    /// `FADE_TYPE_BRIGHTNESS_OUT`: from 0 to −16.
    Out,
}

/// The field's master-brightness fade — `BeginNormalPaletteFade
/// (FADE_BOTH_SCREENS, type, type, RGB_BLACK, steps, framesPerStep)`
/// as `FieldMap_FadeScreen` (`src/field/fieldmap.c:644`),
/// `PaletteFadeUntilFinished` and `CallTask_FadeFromBlack`
/// (`asm/unk_02055244.s:97,:137`) call it, stepped once per tick after
/// the scene's exec.
///
/// The arithmetic is `sub_02010B14`'s (`asm/unk_0201010C.s:1486`): the
/// init writes the start brightness and stores `current = start << 7`,
/// `end = end << 7`, `delta = ((end − start) << 7) / steps` — the
/// second argument of `sub_02010A6C` is the **end** value (`add r1,
/// r6, #0` at `:1553`, r6 holding the end), so an OUT fade to black is
/// a gradual 6-step darkening, as the oracle shows on the stairs
/// (`docs/field-system.md`). `app::fade::BrightnessFade` derives the
/// delta from the begin's colour instead, which happens to agree for
/// the IN-black case only; this type carries the asm's rule for the
/// field's fades and is the reference for fixing the other.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FieldFade {
    flag: bool,
    work_state: u8,
    steps: i32,
    frames_per_step: u8,
    counter: u8,
    current: i32,
    end: i32,
    delta: i32,
    written: i32,
}

impl Default for FieldFade {
    fn default() -> Self {
        Self {
            flag: false,
            work_state: 3,
            steps: 0,
            frames_per_step: 0,
            counter: 0,
            current: 0,
            end: 0,
            delta: 0,
            written: 0,
        }
    }
}

impl FieldFade {
    /// The field's standard fade: 6 steps of 1 frame each.
    pub const STEPS: i32 = 6;
    /// Frames per step of the standard fade.
    pub const FRAMES_PER_STEP: u8 = 1;

    /// `BeginNormalPaletteFade` to or from black: the init's start
    /// brightness is written now; `steps` × `frames_per_step` steps
    /// follow, one per [`update`](Self::update).
    pub fn begin(&mut self, direction: FadeDirection, steps: i32, frames_per_step: u8) {
        let (start, end) = match direction {
            FadeDirection::In => (-16, 0),
            FadeDirection::Out => (0, -16),
        };
        self.flag = true;
        self.work_state = 1;
        self.steps = steps;
        self.frames_per_step = frames_per_step;
        self.counter = 0;
        self.current = start << 7;
        self.end = end << 7;
        self.delta = ((end - start) << 7) / steps;
        self.written = start;
    }

    /// A direct `SetMasterBrightness` on both screens (`sub_0200FBF4`).
    pub fn write(&mut self, value: i32) {
        self.written = value;
    }

    /// `HandleFadeUpdateFrame`: one post-VBlank step.
    pub fn update(&mut self) {
        if !self.flag {
            return;
        }
        match self.work_state {
            1 => {
                self.counter = self.counter.wrapping_add(1);
                if self.counter < self.frames_per_step {
                    return;
                }
                self.counter = 0;
                let next = self.steps - 1;
                if next <= 0 {
                    self.current = self.end;
                    self.written = self.current / 128;
                    self.work_state = 2;
                } else {
                    self.steps = next;
                    self.current += self.delta;
                    self.written = self.current / 128;
                }
            }
            2 => {
                self.work_state = 3;
                self.flag = false;
            }
            _ => {}
        }
    }

    /// `IsPaletteFadeFinished`, as the next exec sees it.
    #[must_use]
    pub fn is_finished(&self) -> bool {
        !self.flag
    }

    /// The last written brightness as the register holds it.
    #[must_use]
    pub fn brightness(&self) -> MasterBrightness {
        let v = self.written;
        if v == 0 {
            MasterBrightness {
                mode: BrightnessMode::Disabled,
                value: 0,
            }
        } else if v > 0 {
            MasterBrightness {
                mode: BrightnessMode::Up,
                value: (v & 0x1F) as u8,
            }
        } else {
            MasterBrightness {
                mode: BrightnessMode::Down,
                value: ((-v) & 0x1F) as u8,
            }
        }
    }
}

// ---------------------------------------------------------------------------
// The player's map-object sprite
// ---------------------------------------------------------------------------

/// Frames per texture of a map-object animation: the class records of
/// `ov01_02207318` end in the word 2 (`asm/overlay_01_sprite_data.s:267`
/// onward) and the hero's 8 animations of 16 frames select its 32
/// textures four frames apiece — measured against the oracle's walk
/// (`docs/field-system.md`).
pub const FRAMES_PER_TEXTURE: i32 = 4;
/// Frames per animation for the hero's class (`ov01_0220715C`: eight
/// records of 16 frames, `0x00-0x0F` … `0x70-0x7F`).
pub const FRAMES_PER_ANIM: i32 = 16;
/// The standing snap's group: `ov01_021F8C30` floors the animation
/// frame to a multiple of 8 (`asm/overlay_01_021F72DC.s:3258`).
pub const STAND_SNAP_FRAMES: i32 = 8;

/// The player's decoded frame strip and animation state — the sprite
/// half of `LocalMapObject` that `ov01_021F772C`
/// (`asm/overlay_01_021F72DC.s:562`) drives every tick: the facing
/// picks the animation (`ov01_02208B70`: N, S, W, E → 0..3), the
/// object's animation group picks the per-tick frame step
/// (`ov01_02208AC0`'s handlers), and the shown texture is the frame
/// over [`FRAMES_PER_TEXTURE`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlayerSprite {
    /// Every texture of the `a/0/8/1` member, stacked top to bottom in
    /// frame order (`hero.1` … `hero.32`): a 32 × (32·n) RGBA8 strip.
    pub strip: Arc<Texture>,
    /// One frame's width and height in texels.
    pub frame_size: (u16, u16),
    /// Textures in the strip.
    pub frame_count: u16,
    /// The facing whose animation is selected (`sub_02023EE0`'s index).
    pub facing: Direction,
    /// The animation frame, fx32, absolute over the strip's frames
    /// (`sprite + 0xB8`).
    pub frame: i32,
}

impl PlayerSprite {
    /// Decodes the player's move-model textures for `gender` through
    /// the sprite table (`ov01_022074A8`: `SPRITE_HERO` / `SPRITE_HEROINE`
    /// → the `a/0/8/1` member), ordered by the numeric suffix of the
    /// texture names.
    ///
    /// # Errors
    /// Returns an [`AssetsError`] when the overlay, the table row or the
    /// texture archive is missing or malformed.
    pub fn load(store: &AssetStore, gender: u8) -> Result<Self, AssetsError> {
        let ov = Ov01::load(store)?;
        let sprite = ov01::player_sprite(gender);
        let mmodel = SpriteModelTable::from_overlay(&ov)?
            .model_for_sprite(sprite)
            .ok_or_else(|| AssetsError::Missing(format!("sprite {sprite} in the model table")))?;
        let narc = store.narc(ov01::MMODEL_NARC)?;
        let bytes = narc_member(&narc, ov01::MMODEL_NARC, usize::from(mmodel))?;
        let what = format!("{}#{mmodel}", ov01::MMODEL_NARC);
        let corrupt = |source: NdsError| AssetsError::Corrupt {
            what: what.clone(),
            source,
        };
        let btx = Btx::parse(&bytes).map_err(corrupt)?;
        let palette = btx
            .palettes()
            .first()
            .ok_or_else(|| {
                corrupt(NdsError::Invalid {
                    what: "player texture archive without a palette",
                })
            })?
            .name()
            .to_owned();
        let mut names: Vec<(u32, String)> = btx
            .textures()
            .iter()
            .map(|t| {
                let n = t
                    .name()
                    .rsplit('.')
                    .next()
                    .and_then(|s| s.parse::<u32>().ok())
                    .unwrap_or(u32::MAX);
                (n, t.name().to_owned())
            })
            .collect();
        names.sort();
        let mut frames = Vec::with_capacity(names.len());
        for (_, name) in &names {
            frames.push(decode_texture(&btx, name, &palette).map_err(corrupt)?);
        }
        let (Some(first), true) = (
            frames.first(),
            frames.iter().all(|f| f.width == frames[0].width),
        ) else {
            return Err(corrupt(NdsError::Invalid {
                what: "player texture archive without uniform frames",
            }));
        };
        let (w, h) = (first.width, first.height);
        let mut pixels = Vec::with_capacity(w * h * frames.len());
        for f in &frames {
            pixels.extend_from_slice(&f.pixels);
        }
        let raw = first
            .raw
            .as_ref()
            .map(|first_raw| super::model::RawTexture {
                texels: frames
                    .iter()
                    .flat_map(|frame| {
                        frame
                            .raw
                            .as_ref()
                            .into_iter()
                            .flat_map(|raw| raw.texels.iter().copied())
                    })
                    .collect(),
                palette: first_raw.palette.clone(),
            });
        Ok(Self {
            strip: Arc::new(Texture {
                width: w,
                height: h * frames.len(),
                pixels,
                format: first.format,
                color0_transparent: first.color0_transparent,
                raw,
            }),
            frame_size: (
                u16::try_from(w).unwrap_or(u16::MAX),
                u16::try_from(h).unwrap_or(u16::MAX),
            ),
            frame_count: u16::try_from(frames.len()).unwrap_or(u16::MAX),
            facing: Direction::South,
            frame: 0,
        })
    }

    /// The first frame (fx32) of `facing`'s animation
    /// (`sub_02024394`: the class record's start, `facing.index() * 16`).
    fn anim_start(facing: Direction) -> i32 {
        (facing.index() as i32 * FRAMES_PER_ANIM) << 12
    }

    /// The last frame (fx32) of `facing`'s animation (the record's end).
    fn anim_end(facing: Direction) -> i32 {
        Self::anim_start(facing) + ((FRAMES_PER_ANIM - 1) << 12)
    }

    /// `sub_02023EE0(anim)` + `sub_02023F40(0)`: select the facing's
    /// animation at its first frame.
    pub fn reset(&mut self, facing: Direction) {
        self.facing = facing;
        self.frame = Self::anim_start(facing);
    }

    /// `sub_02023F04(step)` → `sub_020243C4`: a frame outside the
    /// animation restarts it; otherwise advance, and past the end loop
    /// to the start (the hero's records have no hold flag).
    pub fn advance(&mut self, step: i32) {
        let (start, end) = (Self::anim_start(self.facing), Self::anim_end(self.facing));
        if self.frame < start || self.frame > end {
            self.frame = start;
            return;
        }
        let next = self.frame + step;
        self.frame = if next <= end { next } else { start };
    }

    /// `ov01_021F8C30`: floor the animation-relative frame to a
    /// multiple of eight (the standing pose of the current half-cycle).
    pub fn snap(&mut self) {
        let start = Self::anim_start(self.facing);
        let rel = (self.frame - start) >> 12;
        let rel = rel - rel.rem_euclid(STAND_SNAP_FRAMES);
        self.frame = start + (rel << 12);
        self.advance(0);
    }

    /// `ov01_021F9344` (`asm/overlay_01_021F8D80.s:826`): the animation
    /// holds while the object's movement is paused without a held
    /// command, or while flag 8 is set.
    fn paused(object: &MapObject) -> bool {
        (object.test_flags(flag::UNK30 | flag::MOVEMENT_PAUSED)
            && !object.test_flags(flag::HELD_MOVEMENT))
            || object.test_flags(flag::UNK8)
    }

    /// One tick of `ov01_021F772C` after the object's movement step:
    /// `ended` is the step's `END_MOVEMENT` (the end-of-move callback
    /// `ov01_021F77D0` advances one frame first), then a changed facing
    /// restarts its animation, then the animation group's handler steps
    /// the frame — 0 snaps (`ov01_021F796C`), 1/2 add 0x800
    /// (`ov01_021F79A0`), 3 adds 0x1000 (`ov01_021F79DC`), 4 adds
    /// 0x2000, 5 adds 0x4000 (`ov01_021F7A54`).
    pub fn update(&mut self, object: &MapObject, ended: bool) {
        if ended && !Self::paused(object) {
            self.advance(FX32_ONE);
        }
        if object.current_facing != self.facing {
            self.reset(object.current_facing);
        }
        let paused = Self::paused(object);
        match object.anim_group {
            0 => self.snap(),
            _ if paused => {}
            1 | 2 => self.advance(0x800),
            3 => self.advance(0x1000),
            4 => self.advance(0x2000),
            5 => self.advance(0x4000),
            _ => self.advance(0x1000),
        }
    }

    /// The texture the current frame shows, as an index into the strip.
    #[must_use]
    pub fn texture_index(&self) -> u16 {
        let index = (self.frame >> 12) / FRAMES_PER_TEXTURE;
        u16::try_from(index.max(0))
            .unwrap_or(0)
            .min(self.frame_count.saturating_sub(1))
    }

    /// The billboard for this sprite standing at `position` (the
    /// object's position vector plus [`SPRITE_OFFSET`]).
    #[must_use]
    pub fn view(&self, position: VecFx32) -> ObjectView {
        let (w, h) = self.frame_size;
        ObjectView {
            texture: Arc::clone(&self.strip),
            rect: (0, self.texture_index() * h, w, h),
            world_pos: [
                position.x + SPRITE_OFFSET.x,
                position.y + SPRITE_OFFSET.y,
                position.z + SPRITE_OFFSET.z,
            ],
            size_px: (w, h),
            mirrored: false,
        }
    }
}

/// Where the billboard's anchor sits relative to the object's position
/// vector, fx32. Retail composes the sprite position from the position
/// vector, the facing vector and two per-object offsets
/// (`ov01_021F93AC`, `asm/overlay_01_021F8D80.s:882`) plus six units of
/// height (`ov01_021FA3E8`, `asm/overlay_01_021F944C.s:2243`); against
/// the oracle's bedroom frame the player's frame lands **five pixels
/// lower** than a quad anchored at the tile centre's projection, which
/// this offset reproduces for the field's cameras
/// (`docs/field-system.md`, "The sprite's anchor").
pub const SPRITE_OFFSET: VecFx32 = VecFx32 {
    x: 0,
    y: 0,
    z: 6 * FX32_ONE + FX32_ONE / 2,
};

// ---------------------------------------------------------------------------
// Terrain as the movement code sees it
// ---------------------------------------------------------------------------

/// pret's `sMetatileBehaviorFlags` (`src/metatile_behavior.c:7`): one
/// byte per behaviour — bit 0 `TILE_BEHAVIOR_FLAG_SURFABLE`, bit 1
/// `TILE_BEHAVIOR_FLAG_ENCOUNTER` — read from the ARM9 image at
/// [`Self::ADDRESS`] (located by its 237-byte pattern; unique in the
/// retail US image). `MetatileBehavior_IsSurfableWater` is what keeps
/// the player off New Bark Town's pond, whose tiles carry behaviour 21
/// (`WATER_SEA`) with bit 15 clear.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BehaviorFlags(Vec<u8>);

impl BehaviorFlags {
    /// The table's RAM address in the retail US ARM9 image.
    pub const ADDRESS: u32 = 0x020F_CA74;
    /// Entries: `TILE_BEHAVIOR_0` … `TILE_BEHAVIOR_236`.
    pub const LEN: usize = 237;

    /// Reads the table out of the ARM9 image.
    ///
    /// # Errors
    /// Returns an [`AssetsError`] when the image cannot be expanded or is
    /// shorter than the table's end.
    pub fn load(store: &AssetStore) -> Result<Self, AssetsError> {
        let image = store.arm9_image()?;
        let start = usize::try_from(Self::ADDRESS - store.arm9_base()).map_err(|_| {
            AssetsError::Missing("metatile behaviour flags below the ARM9 base".into())
        })?;
        let bytes = image.get(start..start + Self::LEN).ok_or_else(|| {
            AssetsError::Missing("metatile behaviour flags past the ARM9 image".into())
        })?;
        Ok(Self(bytes.to_vec()))
    }

    /// The flags byte of `behavior` (0 past the table).
    #[must_use]
    pub fn flags(&self, behavior: u8) -> u8 {
        self.0.get(usize::from(behavior)).copied().unwrap_or(0)
    }

    /// `MetatileBehavior_IsSurfableWater`: bit 0.
    #[must_use]
    pub fn surfable(&self, behavior: u8) -> bool {
        self.flags(behavior) & 1 != 0
    }

    /// `MetatileBehavior_IsEncounterTile` (`:459`): bit 1.
    #[must_use]
    pub fn encounter(&self, behavior: u8) -> bool {
        self.flags(behavior) & 2 != 0
    }
}

/// The loaded map's terrain as the collision and height code read it
/// (`GetMetatileBehavior` / `sub_020548C0` / `sub_02054940`): a tile
/// whose cell is not resident answers [`ATTR_NONE`]; surfable water
/// comes from the ROM's behaviour-flags table; heights come from the
/// resident cells' BDHC plates.
#[derive(Debug, Clone, Copy)]
pub struct SceneTerrain<'a> {
    /// The scene's attribute window.
    pub attrs: &'a TerrainAttributes,
    /// The behaviour flags table.
    pub flags: &'a BehaviorFlags,
    /// The resident cells, for their height data.
    pub cells: &'a [LandCell],
}

impl Collision for SceneTerrain<'_> {
    fn attr(&self, x: i32, z: i32) -> u16 {
        self.attrs.attr(x, z).unwrap_or(ATTR_NONE)
    }

    fn surfable(&self, x: i32, z: i32) -> bool {
        self.flags.surfable(self.behavior(x, z))
    }

    /// `sub_02054774`: the plate nearest the current height.
    fn height_at(&self, x: i32, y: i32, z: i32) -> Option<i32> {
        scene_height(self.cells, HeightMode::Nearest, x, y, z)
    }
}

/// A warp whose destination map failed to load (`change_map`): the
/// transition is abandoned on the current map instead of unwinding
/// the game, and the failure is reported here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MapLoadError {
    /// The destination that failed.
    pub destination: Location,
    /// The asset error's message.
    pub message: String,
}

// ---------------------------------------------------------------------------
// Map transitions
// ---------------------------------------------------------------------------

/// Which map-exit/enter routine pair a transition runs — pret's
/// `transitionNo` into `sMapExitRoutines` / `sMapEnterRoutines`
/// (`asm/unk_02055BF0_data.s:25`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TransitionKind {
    /// `transitionNo` 0/4/5/6 — a warp-entrance or warp tile
    /// (`sub_02055CD8`): fade out, fade in, walk one tile off the
    /// entrance.
    Entrance,
    /// `transitionNo` 1 — a door faced from outside (`sub_02056040` /
    /// `sub_020565FC`): walk into the door, fade; fade in on the
    /// doormat and walk one tile in.
    Door,
    /// `transitionNo` 3 — warp stairs (`sub_0205613C` / `sub_020566F8`):
    /// walk one tile into the stairs, fade; arrive one tile inside the
    /// wall facing back and walk out onto the stairs while fading in.
    Stairs,
}

/// Where a running transition is (`docs/field-system.md`, "Warps").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TransitionStage {
    /// The task's prologue states and the exit routine's own, before
    /// anything moves.
    Exit,
    /// The screen is black; the destination map loads and the player
    /// is placed.
    Black,
    /// The enter routine: fading in, the arrival walk.
    Enter,
}

/// A map transition in progress — `sub_02055DBC`'s environment with
/// the measured tick schedule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Transition {
    /// The routine pair.
    pub kind: TransitionKind,
    /// The destination (`FieldTransitionEnvironment.location`).
    pub destination: Location,
    /// Ticks since the task was created (the trigger tick is 0).
    pub tick: u32,
    /// The stage.
    pub stage: TransitionStage,
    /// Whether the destination has been loaded and the player placed.
    pub loaded: bool,
}

impl Transition {
    /// Tick at which the exit walk's held movement is set — the task's
    /// states 0→1, 1→2, 2 (exit routine pushed), the routine's state 0
    /// (follow-Pokémon check) and state 1 (`MapObject_SetHeldMovement`),
    /// one per tick (`sub_02055DBC`, `sub_0205613C`).
    pub const EXIT_WALK_TICK: u32 = 4;
    /// Ticks of the exit walk: `MovementCmd::WalkSlower*`, 16 frames.
    pub const EXIT_WALK_TICKS: u32 = 16;
    /// Tick of the fade-out's begin for the walking exits: the walk's
    /// done flag is seen the tick after its last step and the fade
    /// begins the tick after that (`sub_0205613C` states 2 and 3).
    pub const WALK_EXIT_FADE_TICK: u32 = Self::EXIT_WALK_TICK + Self::EXIT_WALK_TICKS + 1;
    /// Tick of the fade-out's begin for the entrance exit
    /// (`sub_02056004` state 0: `PaletteFadeUntilFinished` right after
    /// the task's prologue).
    pub const ENTRANCE_EXIT_FADE_TICK: u32 = 3;
    /// Ticks from the fade-out's begin to the fade-in's begin — the
    /// six fade steps, the field overlay's exit, the map load, the
    /// overlay's reload and the bottom screen's start, measured as 88
    /// VBlanks on the oracle's bedroom→1F stairs (5132 → 5220).
    pub const FADE_TO_FADE_TICKS: u32 = 44;
    /// Tick within the black period at which the destination loads and
    /// the player is placed (the exit task returns when its fade's
    /// flag clears: begin + 7; the map load follows).
    pub const LOAD_AFTER_FADE_TICKS: u32 = 7;
    /// Ticks of the arrival walk (`MovementCmd::WalkSlower*`).
    pub const ENTER_WALK_TICKS: u32 = 16;
    /// Ticks after the fade-in's begin until movement is allowed again:
    /// the walk's done flag seen at +16, the fade's finish at +17 (the
    /// routine returns, the task unwinds), control at +18.
    pub const ENTER_TICKS: u32 = Self::ENTER_WALK_TICKS + 2;

    /// The tick the fade-out begins.
    #[must_use]
    pub fn fade_out_tick(&self) -> u32 {
        match self.kind {
            TransitionKind::Entrance => Self::ENTRANCE_EXIT_FADE_TICK,
            TransitionKind::Door | TransitionKind::Stairs => Self::WALK_EXIT_FADE_TICK,
        }
    }

    /// The tick the fade-in begins.
    #[must_use]
    pub fn fade_in_tick(&self) -> u32 {
        self.fade_out_tick() + Self::FADE_TO_FADE_TICKS
    }

    /// The tick the task finishes (movement is allowed from the next).
    #[must_use]
    pub fn end_tick(&self) -> u32 {
        self.fade_in_tick() + Self::ENTER_TICKS - 1
    }
}

/// What `FieldInput_Process` started this tick, or would have — the
/// seam the NPC, script and menu workstreams fill in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FieldEvent {
    /// A map transition began (`NewFieldTransitionEnvironment` /
    /// `sub_02055CD8`); movement stops.
    Warp(TransitionKind),
    /// A pressed while standing (`fieldInput->interact`): the object,
    /// background-event and metatile scripts would be looked up here.
    /// Nothing consumes it yet.
    Interact,
    /// X / the touch menu asked for the start menu (`fieldInput->menu`).
    /// Nothing consumes it yet.
    Menu,
}

/// The field system's phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FieldPhase {
    /// The new-game entry task: the map is up, fading in from black
    /// (`sub_020553C0`, `asm/unk_02055244.s:213`).
    FadeIn,
    /// Free movement.
    Running,
    /// A map transition task is running.
    Transition,
}

// ---------------------------------------------------------------------------
// The system
// ---------------------------------------------------------------------------

/// pret's `FieldSystem` for the live overworld: the loaded scene, the
/// player avatar with its map object and sprite, the location and the
/// entrance it came through, the camera preset, the fade, and the task
/// in progress.
#[derive(Debug)]
pub struct FieldSystem {
    scene: Arc<FieldScene>,
    flags: BehaviorFlags,
    avatar: PlayerAvatar,
    sprite: PlayerSprite,
    gender: u8,
    location: Location,
    entrance: Location,
    previous: Location,
    camera: CameraPreset,
    lighting: ModelLighting,
    area_lights: AreaLightManager,
    clock_seconds: u32,
    fade: FieldFade,
    phase: FieldPhase,
    task_tick: u32,
    transition: Option<Transition>,
    encounter_steps: EncounterSteps,
    last_touch_menu_input: u16,
    last_input: Option<FieldInput>,
    last_outcome: Option<MoveOutcome>,
    last_event: Option<FieldEvent>,
    last_error: Option<MapLoadError>,
    ticks: u32,
    frame: LogicalFrame,
}

impl FieldSystem {
    /// `CallFieldTask_NewGame` (`src/field_warp_tasks.c:409`) → the field
    /// at `Location::PLAYER_ROOM` with the new game's blank
    /// `PlayerSaveData` (no running shoes), fading in from black on the
    /// first tick.
    ///
    /// # Errors
    /// Returns an [`AssetsError`] when a map or sprite member fails to
    /// load.
    pub fn new_game(store: &AssetStore, gender: u8) -> Result<Self, AssetsError> {
        Self::enter(store, Location::PLAYER_ROOM, gender)
    }

    /// New-game entry with a deterministic time of day for lighting.
    ///
    /// # Errors
    /// Returns an error if the scene, sprite or light archive cannot load.
    pub fn new_game_at(
        store: &AssetStore,
        gender: u8,
        seconds_of_day: u32,
    ) -> Result<Self, AssetsError> {
        Self::enter_at(store, Location::PLAYER_ROOM, gender, seconds_of_day)
    }

    /// The field entered at `location` — `sub_02052F94` + `sub_02053284`
    /// + `sub_0205316C` (`src/field_warp_tasks.c:197-291`): resolve a warp
    /// id to its tile, load the map around the player, create the
    /// avatar there — then the new-game fade-in task.
    ///
    /// # Errors
    /// Returns an [`AssetsError`] when a map or sprite member fails to
    /// load.
    pub fn enter(store: &AssetStore, location: Location, gender: u8) -> Result<Self, AssetsError> {
        Self::enter_at(store, location, gender, 9 * 60 * 60)
    }

    /// Enters a scene with a deterministic time of day for lighting.
    ///
    /// # Errors
    /// Returns an error if the scene, sprite or light archive cannot load.
    pub fn enter_at(
        store: &AssetStore,
        location: Location,
        gender: u8,
        seconds_of_day: u32,
    ) -> Result<Self, AssetsError> {
        let sprite = PlayerSprite::load(store, gender)?;
        let flags = BehaviorFlags::load(store)?;
        let (scene, location) = Self::load_location(store, location, gender)?;
        let avatar = Self::create_avatar(&scene, &flags, &location, gender);
        let camera = scene.camera;
        let mut lighting = ModelLighting::default();
        let archive = AreaLightArchive::load(
            store,
            archive_for_light_type(scene.area.light_selector, false),
        )?;
        let area_lights = AreaLightManager::new(archive, seconds_of_day, &mut lighting);
        let mut system = Self {
            scene: Arc::new(scene),
            flags,
            avatar,
            sprite,
            gender,
            location,
            entrance: location,
            previous: location,
            camera,
            lighting,
            area_lights,
            clock_seconds: seconds_of_day % 86_400,
            fade: FieldFade::default(),
            phase: FieldPhase::FadeIn,
            task_tick: 0,
            transition: None,
            encounter_steps: EncounterSteps::default(),
            last_touch_menu_input: 0,
            last_input: None,
            last_outcome: None,
            last_event: None,
            last_error: None,
            ticks: 0,
            frame: LogicalFrame::default(),
        };
        system.sprite.reset(location.direction);
        // Black until the fade-in's first step (sub_0200FBF4 at the
        // field's construction).
        system.fade.write(-16);
        system.frame = system.snapshot();
        Ok(system)
    }

    /// `sub_02052F94`'s warp resolution and `FieldSystem_CreateMap`'s
    /// load: the destination map with the player at the location's tile
    /// (the target warp's, when `warp_id` names one).
    fn load_location(
        store: &AssetStore,
        mut location: Location,
        gender: u8,
    ) -> Result<(FieldScene, Location), AssetsError> {
        if location.warp_id >= 0 {
            let headers = MapHeaders::load(store)?;
            let header = headers
                .get(location.map_id)
                .ok_or_else(|| AssetsError::Missing(format!("map header {}", location.map_id)))?;
            let events = super::events::MapEvents::load(store, header.events_bank)?;
            let warp: &WarpEvent =
                events.warps.get(location.warp_id as usize).ok_or_else(|| {
                    AssetsError::Missing(format!(
                        "warp {} of map {}",
                        location.warp_id, location.map_id
                    ))
                })?;
            location.x = i32::from(warp.x);
            location.z = i32::from(warp.z);
        }
        let scene = FieldScene::load(store, location.map_id, location.x, location.z, gender)?;
        Ok((scene, location))
    }

    /// `PlayerAvatar_CreateWithParams(x, y, direction, state, gender,
    /// sprite, playerSaveData)` for the new game's save data. The
    /// object is created with `HEIGHT_STALE` (`sub_0205EC90`) and, the
    /// field overlay being up, its height is fetched at once
    /// (`sub_0205EFB4` → `sub_0205EF8C`, `src/map_object.c:217,805`):
    /// the player stands on the land surface the cells' BDHC plates
    /// give (`MapObject::update_height_from`).
    fn create_avatar(
        scene: &FieldScene,
        flags: &BehaviorFlags,
        location: &Location,
        gender: u8,
    ) -> PlayerAvatar {
        let mut avatar = PlayerAvatar::new(
            location.x,
            location.z,
            location.direction,
            PlayerState::Walking,
            u32::from(gender),
            u32::from(ov01::player_sprite(gender)),
            PlayerSaveData::default(),
        );
        let terrain = SceneTerrain {
            attrs: &scene.terrain,
            flags,
            cells: &scene.cells,
        };
        avatar.object.update_height_from(&terrain);
        avatar
    }

    /// The loaded scene.
    #[must_use]
    pub fn scene(&self) -> &Arc<FieldScene> {
        &self.scene
    }

    /// The player avatar (its map object holds the tile and position).
    #[must_use]
    pub fn avatar(&self) -> &PlayerAvatar {
        &self.avatar
    }

    /// The player's sprite state.
    #[must_use]
    pub fn sprite(&self) -> &PlayerSprite {
        &self.sprite
    }

    /// The ROM's behaviour flags table.
    #[must_use]
    pub fn behavior_flags(&self) -> &BehaviorFlags {
        &self.flags
    }

    /// The loaded scene's terrain as the movement code collides with it.
    #[must_use]
    pub fn terrain(&self) -> SceneTerrain<'_> {
        SceneTerrain {
            attrs: &self.scene.terrain,
            flags: &self.flags,
            cells: &self.scene.cells,
        }
    }

    /// `fieldSystem->location`: the map, warp and tile the player is on.
    #[must_use]
    pub fn location(&self) -> Location {
        self.location
    }

    /// `LocalFieldData.entrancePosition`: the warp the player last came
    /// through, saved by `FieldSystem_MapConnection`.
    #[must_use]
    pub fn entrance(&self) -> Location {
        self.entrance
    }

    /// `LocalFieldData.previousPosition`: the location before the last
    /// map change.
    #[must_use]
    pub fn previous(&self) -> Location {
        self.previous
    }

    /// The camera preset in force.
    #[must_use]
    pub fn camera(&self) -> CameraPreset {
        self.camera
    }

    /// The phase.
    #[must_use]
    pub fn phase(&self) -> FieldPhase {
        self.phase
    }

    /// The transition in progress, if any.
    #[must_use]
    pub fn transition(&self) -> Option<Transition> {
        self.transition
    }

    /// The fade state.
    #[must_use]
    pub fn fade(&self) -> &FieldFade {
        &self.fade
    }

    /// `FieldSystem_IsPlayerMovementAllowed`: the map is up and no field
    /// task is running.
    #[must_use]
    pub fn movement_allowed(&self) -> bool {
        self.phase == FieldPhase::Running
    }

    /// The last tick's input digest, when movement was allowed.
    #[must_use]
    pub fn last_input(&self) -> Option<FieldInput> {
        self.last_input
    }

    /// The last tick's `MoveControl` decision, when it ran.
    #[must_use]
    pub fn last_outcome(&self) -> Option<MoveOutcome> {
        self.last_outcome
    }

    /// The event `FieldInput_Process` started (or noted) last tick.
    #[must_use]
    pub fn last_event(&self) -> Option<FieldEvent> {
        self.last_event
    }

    /// The last warp whose destination failed to load, if any: the
    /// field abandoned that transition and kept the current map.
    #[must_use]
    pub fn last_error(&self) -> Option<&MapLoadError> {
        self.last_error.as_ref()
    }

    /// The step counters the encounter check reads.
    #[must_use]
    pub fn encounter_steps(&self) -> EncounterSteps {
        self.encounter_steps
    }

    /// Ticks run.
    #[must_use]
    pub fn ticks(&self) -> u32 {
        self.ticks
    }

    /// The frame the last tick produced.
    #[must_use]
    pub fn frame(&self) -> &LogicalFrame {
        &self.frame
    }

    /// The player's position vector — the camera's fixed target.
    #[must_use]
    pub fn camera_target(&self) -> [i32; 3] {
        let p = self.avatar.object.position;
        [p.x, p.y, p.z]
    }

    /// One game tick (see the module doc for the order).
    pub fn tick(&mut self, input: Input, store: &AssetStore) -> &LogicalFrame {
        let seconds = (self.clock_seconds + self.ticks / 60) % 86_400;
        self.area_lights.update(seconds, &mut self.lighting);
        let previous_keys = self.last_input.map_or(Keys::IDLE, |i| i.held_keys);
        let held = input.keys;
        let new_keys = held.pressed(previous_keys);

        // 1. FieldSystem_Control.
        self.last_event = None;
        self.last_outcome = None;
        if self.movement_allowed() {
            self.avatar.update_movement();
            let digest = FieldInput::update(
                &self.avatar,
                &mut self.last_touch_menu_input,
                new_keys,
                held,
                FieldInputContext::default(),
            );
            self.last_input = Some(digest);
            let event = self.process_input(&digest);
            if let Some(FieldEvent::Warp(_)) = event {
                // sub_0205CF44: the avatar stops for the event.
                self.avatar.move_state = AvatarMoveState::None;
                self.avatar.player_move_state = PlayerMoveState::None;
                self.avatar.clear_gear_and_flag2();
            } else {
                let terrain = SceneTerrain {
                    attrs: &self.scene.terrain,
                    flags: &self.flags,
                    cells: &self.scene.cells,
                };
                let outcome = self.avatar.move_control(&digest, &terrain);
                self.last_outcome = Some(outcome);
            }
            self.last_event = event;
        } else {
            self.last_input = Some(FieldInput {
                held_keys: held,
                new_keys,
                ..FieldInput::default()
            });
        }

        // 2. FieldSystem_RunTaskFrame.
        self.run_task_frame(store);

        // 3. The field overlay's frame: camera at the position vector,
        //    the objects as the previous step left them.
        let mut frame = self.snapshot();

        // 4. The object SysTask: movement step, then the sprite update.
        let report = {
            let terrain = SceneTerrain {
                attrs: &self.scene.terrain,
                flags: &self.flags,
                cells: &self.scene.cells,
            };
            self.avatar.object.tick(&terrain)
        };
        self.sprite.update(&self.avatar.object, report.ended);
        if let Some(input) = self.last_input.filter(|_| self.movement_allowed()) {
            if input.end_movement {
                self.encounter_steps.on_end_movement();
            }
        }

        // 5. HandleFadeUpdateFrame, then the register the frame shows.
        self.fade.update();
        let brightness = self.fade.brightness();
        frame.main.brightness = brightness;
        frame.sub.brightness = brightness;
        self.ticks += 1;
        self.frame = frame;
        &self.frame
    }

    /// The task stack, one state per tick: the new-game fade-in
    /// (`sub_020553C0`) or the transition (`sub_02055DBC`).
    fn run_task_frame(&mut self, store: &AssetStore) {
        match self.phase {
            FieldPhase::FadeIn => {
                // sub_020553C0 state 1: CallTask_FadeFromBlack — a 6 × 1
                // IN fade; its poll reads true the tick after the flag
                // clears (begin + 7), when the task unwinds; movement is
                // allowed from the following tick.
                if self.task_tick == 0 {
                    self.fade.begin(
                        FadeDirection::In,
                        FieldFade::STEPS,
                        FieldFade::FRAMES_PER_STEP,
                    );
                }
                if self.task_tick >= 7 {
                    self.phase = FieldPhase::Running;
                }
                self.task_tick += 1;
            }
            FieldPhase::Running => {}
            FieldPhase::Transition => {
                let Some(mut t) = self.transition else {
                    self.phase = FieldPhase::Running;
                    return;
                };
                let fade_out = t.fade_out_tick();
                let fade_in = t.fade_in_tick();
                match t.kind {
                    TransitionKind::Stairs | TransitionKind::Door
                        if t.tick == Transition::EXIT_WALK_TICK =>
                    {
                        // sub_0205613C state 1 / sub_02056040's door walk:
                        // one WalkSlower tile in the facing direction.
                        let dir = self.avatar.facing();
                        self.hold(MovementCmd::for_direction(family::WALK_SLOWER, dir));
                    }
                    _ => {}
                }
                if t.tick == fade_out {
                    self.avatar.object.clear_held_movement_if_idle();
                    self.fade.begin(
                        FadeDirection::Out,
                        FieldFade::STEPS,
                        FieldFade::FRAMES_PER_STEP,
                    );
                }
                if t.tick == fade_out + Transition::LOAD_AFTER_FADE_TICKS && !t.loaded {
                    t.stage = TransitionStage::Black;
                    if let Err(error) = self.change_map(store, t.destination, t.kind) {
                        // No retail counterpart (a missing member is a
                        // broken ROM there): the transition is
                        // abandoned, the current map fades back in and
                        // the player stays put; `last_error` reports it.
                        self.last_error = Some(MapLoadError {
                            destination: t.destination,
                            message: error.to_string(),
                        });
                        self.avatar.object.clear_held_movement_if_idle();
                        self.fade.begin(
                            FadeDirection::In,
                            FieldFade::STEPS,
                            FieldFade::FRAMES_PER_STEP,
                        );
                        self.transition = None;
                        self.phase = FieldPhase::Running;
                        return;
                    }
                    t.loaded = true;
                }
                if t.tick == fade_in {
                    t.stage = TransitionStage::Enter;
                    self.fade.begin(
                        FadeDirection::In,
                        FieldFade::STEPS,
                        FieldFade::FRAMES_PER_STEP,
                    );
                    let dir = self.avatar.facing();
                    self.hold(MovementCmd::for_direction(family::WALK_SLOWER, dir));
                }
                if t.tick >= t.end_tick() {
                    self.avatar.object.clear_held_movement_if_idle();
                    self.transition = None;
                    self.phase = FieldPhase::Running;
                    return;
                }
                t.tick += 1;
                self.transition = Some(t);
            }
        }
    }

    /// `MapObject_SetHeldMovement` on the player's object.
    fn hold(&mut self, command: Option<MovementCmd>) {
        if let Some(command) = command {
            let _ = self.avatar.object.set_held_movement(command);
        }
    }

    /// The map change inside a transition — `sub_02053740`
    /// (`src/field_warp_tasks.c:558`): the objects and player go, the
    /// destination loads (`sub_02052F94` resolves the warp), the player
    /// is created at the location facing the transition's direction;
    /// then the enter routine's placement for stairs (`sub_02056AEC`,
    /// `asm/unk_02056680.s:563`): one tile into the wall behind the
    /// stairs, facing back toward them.
    ///
    /// # Errors
    /// Returns the [`AssetsError`] of a destination that fails to load,
    /// with the field unchanged.
    fn change_map(
        &mut self,
        store: &AssetStore,
        destination: Location,
        kind: TransitionKind,
    ) -> Result<(), AssetsError> {
        let (scene, location) = Self::load_location(store, destination, self.gender)?;
        let mut lighting = ModelLighting::default();
        let archive = AreaLightArchive::load(
            store,
            archive_for_light_type(scene.area.light_selector, false),
        )?;
        self.area_lights = AreaLightManager::new(
            archive,
            (self.clock_seconds + self.ticks / 60) % 86_400,
            &mut lighting,
        );
        self.lighting = lighting;
        self.previous = self.location;
        self.camera = scene.camera;
        self.scene = Arc::new(scene);
        let mut location = location;
        let mut avatar = Self::create_avatar(&self.scene, &self.flags, &location, self.gender);
        if kind == TransitionKind::Stairs {
            let behavior = self.scene.terrain.behavior(location.x, location.z);
            let (dx, facing) = if behavior == BEHAVIOR_WARP_STAIRS_EAST {
                (1, Direction::West)
            } else if behavior == BEHAVIOR_WARP_STAIRS_WEST {
                (-1, Direction::East)
            } else {
                (0, location.direction)
            };
            // The shifted position vector takes the ground height there
            // (sub_02054940 on the wall tile; 0 when no plate covers
            // it), then sub_0205C810:
            // MapObject_SetPositionFromVectorAndDirection (tile y =
            // (y >> 3) / FX32_ONE) and both move states cleared.
            let x = location.x + dx;
            let mut position = VecFx32::from_tile(x, 0, location.z);
            position.y = self
                .terrain()
                .height_at(position.x, avatar.object.position.y, position.z)
                .unwrap_or(0);
            avatar.object.current = [x, (position.y >> 3) / FX32_ONE, location.z];
            avatar.object.previous = avatar.object.current;
            avatar.object.initial = avatar.object.current;
            avatar.object.position = position;
            avatar.object.set_facing_direction_direct(facing);
            avatar.object.set_next_facing_direction(facing);
            avatar.object.initial_facing = facing;
            avatar.move_state = AvatarMoveState::None;
            avatar.player_move_state = PlayerMoveState::None;
            location.direction = facing;
        }
        self.avatar = avatar;
        self.sprite.reset(location.direction);
        self.location = location;
        // sub_02053038: the step counters restart on a map change.
        self.encounter_steps.reset();
        Ok(())
    }

    /// The frame for this tick: engine A's BG0 bound to the field at
    /// priority 1 (`fieldmap.c:749`), the camera at the player's
    /// position vector, the player billboard; engine B black (the touch
    /// menu is the menus workstream).
    fn snapshot(&self) -> LogicalFrame {
        let mut frame = LogicalFrame::default();
        let target = self.camera_target();
        frame.main.field = Some(FieldFrame {
            scene: Arc::clone(&self.scene),
            camera: self.camera,
            camera_target: target,
            lighting: Some(self.lighting),
            objects: vec![self.sprite.view(self.avatar.object.position)],
        });
        frame.main.bgs[0].enabled = true;
        frame.main.bgs[0].priority = 1;
        let brightness = self.fade.brightness();
        frame.main.brightness = brightness;
        frame.sub.brightness = brightness;
        frame
    }

    // -----------------------------------------------------------------
    // FieldInput_Process (src/field/field_control.c:193), the subset
    // -----------------------------------------------------------------

    /// The part of `FieldInput_Process` that exists: the step handler's
    /// transition check (`FieldSystem_ProcessStep` →
    /// `FieldSystem_CheckTransition`, `:484,:706`), the held-direction
    /// transition check (`FieldSystem_CheckMapTransition`, `:504`), and
    /// the interact/menu seams. Returns the event that consumed the
    /// tick, if any.
    fn process_input(&mut self, input: &FieldInput) -> Option<FieldEvent> {
        if input.movement {
            if let Some(t) = self.check_transition_on_step() {
                return Some(self.start_transition(t));
            }
        }
        // endMovement: wild encounters and signposts — deferred.
        if input.interact {
            // Object, background-event and metatile scripts: no targets
            // are known yet, so the tick is not consumed.
            self.last_event = Some(FieldEvent::Interact);
        }
        if input.map_transition {
            if let Some(t) = self.check_map_transition(input) {
                return Some(self.start_transition(t));
            }
        }
        if input.menu {
            self.last_event = Some(FieldEvent::Menu);
        }
        None
    }

    /// `NewFieldTransitionEnvironment` / `sub_02055CD8`: the task is
    /// created; its first state runs in this tick's
    /// [`run_task_frame`](Self::run_task_frame), which follows.
    fn start_transition(&mut self, transition: Transition) -> FieldEvent {
        self.transition = Some(transition);
        self.phase = FieldPhase::Transition;
        self.task_tick = 0;
        FieldEvent::Warp(transition.kind)
    }

    /// A transition started from outside the input path — the seam for
    /// scripted warps (`sub_02055CD8` from a script's warp command):
    /// `destination` as `FieldSystem_MapConnection` would have built it,
    /// the routine pair `kind`. Runs the same schedule as a warp tile;
    /// refused (`false`) while movement is not allowed. The avatar
    /// stops as it does for a warp event (`sub_0205CF44`).
    pub fn warp(&mut self, destination: Location, kind: TransitionKind) -> bool {
        if !self.movement_allowed() {
            return false;
        }
        self.avatar.move_state = AvatarMoveState::None;
        self.avatar.player_move_state = PlayerMoveState::None;
        self.avatar.clear_gear_and_flag2();
        let event = self.start_transition(Transition {
            kind,
            destination,
            tick: 0,
            stage: TransitionStage::Exit,
            loaded: false,
        });
        self.last_event = Some(event);
        true
    }

    /// `FieldSystem_MapConnection` (`:909`): the warp event on tile
    /// `(x, z)`, as a destination location facing south (`SetLocation`'s
    /// direction 1); the entrance is recorded. Dynamic-warp anchors
    /// (`0x100`) need the save's dynamic warp — not carried yet.
    fn map_connection(&mut self, x: i32, z: i32) -> Option<Location> {
        let (ux, uz) = (u16::try_from(x).ok()?, u16::try_from(z).ok()?);
        let index = self.scene.events.warp_at(ux, uz)?;
        let warp = self.scene.events.warps[index];
        if warp.anchor == DYNAMIC_WARP_ANCHOR {
            return None;
        }
        let location = Location::new(
            warp.header,
            i32::from(warp.anchor),
            i32::from(warp.x),
            i32::from(warp.z),
            Direction::South,
        );
        self.entrance = Location::new(
            self.location.map_id,
            index as i32,
            x,
            z,
            self.avatar.facing(),
        );
        Some(location)
    }

    /// The tile ahead of the player (`PlayerAvatar_GetFacingTileCoords`).
    fn facing_tile(&self) -> (i32, i32) {
        let d = self.avatar.facing();
        (
            self.avatar.x().wrapping_add(d.delta_x()),
            self.avatar.z().wrapping_add(d.delta_z()),
        )
    }

    /// `sub_02055CD8`'s transition: the number it derives from the two
    /// maps' types (0, 4, 5 or 6) selects routines that all fade
    /// without a walk, so one kind covers them.
    fn entrance_transition(&self, destination: Location) -> Transition {
        Transition {
            kind: TransitionKind::Entrance,
            destination,
            tick: 0,
            stage: TransitionStage::Exit,
            loaded: false,
        }
    }

    /// `FieldSystem_CheckTransition` (`:706`) on the tile a step just
    /// landed on: warp-entrance-north / warp-north tiles start the
    /// entrance transition facing north; escalators, warp panels and
    /// ladders are not ported.
    fn check_transition_on_step(&mut self) -> Option<Transition> {
        let (x, z) = (self.avatar.x(), self.avatar.z());
        let behavior = self.scene.terrain.behavior(x, z);
        let mut location = self.map_connection(x, z)?;
        match behavior {
            BEHAVIOR_WARP_ENTRANCE_NORTH | BEHAVIOR_WARP_NORTH => {
                location.direction = Direction::North;
                Some(self.entrance_transition(location))
            }
            BEHAVIOR_ESCALATOR_FLIP_FACE
            | BEHAVIOR_ESCALATOR
            | BEHAVIOR_WARP_PANEL
            | BEHAVIOR_LADDER_DOWN
            | _ => None,
        }
    }

    /// `FieldSystem_CheckMapTransition` (`:504`) while a direction is held
    /// standing still: ladders (not ported); a door faced from outside
    /// (the tile ahead impassable, a warp there, `DOOR`); the standing
    /// tile's warp entrances, warps and stairs in their own direction.
    fn check_map_transition(&mut self, input: &FieldInput) -> Option<Transition> {
        let dir = input.transition_dir?;
        let (x, z) = (self.avatar.x(), self.avatar.z());
        let standing = self.scene.terrain.behavior(x, z);
        if standing == BEHAVIOR_LADDER_NORTH || standing == BEHAVIOR_LADDER_SOUTH {
            return None;
        }
        let (fx, fz) = self.facing_tile();
        if !self.scene.terrain.impassable(fx, fz) {
            return None;
        }
        if self.scene.terrain.behavior(fx, fz) == BEHAVIOR_DOOR {
            if let Some(mut location) = self.map_connection(fx, fz) {
                location.direction = dir;
                return Some(Transition {
                    kind: TransitionKind::Door,
                    destination: location,
                    tick: 0,
                    stage: TransitionStage::Exit,
                    loaded: false,
                });
            }
        }
        let required = match standing {
            BEHAVIOR_WARP_ENTRANCE_EAST | BEHAVIOR_WARP_EAST | BEHAVIOR_WARP_STAIRS_EAST => {
                Some(Direction::East)
            }
            BEHAVIOR_WARP_ENTRANCE_WEST | BEHAVIOR_WARP_WEST | BEHAVIOR_WARP_STAIRS_WEST => {
                Some(Direction::West)
            }
            BEHAVIOR_WARP_ENTRANCE_SOUTH | BEHAVIOR_WARP_SOUTH => Some(Direction::South),
            _ => None,
        };
        if let Some(required) = required {
            if dir != required {
                return None;
            }
        }
        let mut location = self.map_connection(x, z)?;
        location.direction = dir;
        match standing {
            BEHAVIOR_DOOR => Some(Transition {
                kind: TransitionKind::Door,
                destination: location,
                tick: 0,
                stage: TransitionStage::Exit,
                loaded: false,
            }),
            BEHAVIOR_WARP_STAIRS_EAST | BEHAVIOR_WARP_STAIRS_WEST => Some(Transition {
                kind: TransitionKind::Stairs,
                destination: location,
                tick: 0,
                stage: TransitionStage::Exit,
                loaded: false,
            }),
            BEHAVIOR_WARP_ENTRANCE_EAST
            | BEHAVIOR_WARP_EAST
            | BEHAVIOR_WARP_ENTRANCE_WEST
            | BEHAVIOR_WARP_WEST
            | BEHAVIOR_WARP_ENTRANCE_SOUTH
            | BEHAVIOR_WARP_SOUTH => Some(self.entrance_transition(location)),
            _ => None,
        }
    }
}

/// The world position of the centre of tile `(x, z)` at y 0 — the
/// camera target of a standing player.
#[must_use]
pub fn tile_centre(x: i32, z: i32) -> [i32; 3] {
    [
        x * TILE_FX32 + TILE_FX32 / 2,
        0,
        z * TILE_FX32 + TILE_FX32 / 2,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn down(value: u8) -> MasterBrightness {
        MasterBrightness {
            mode: BrightnessMode::Down,
            value,
        }
    }

    #[test]
    fn out_fade_to_black_is_gradual() {
        // sub_02010B14: delta = ((end - start) << 7) / 6 = -341 for an
        // OUT-black fade; current walks -341, -682, -1023, -1364, -1705
        // and the register (current / 128, truncated) -2, -5, -7, -10,
        // -13, then the end snap -16 — the oracle's six darkening
        // steps on the stairs.
        let mut fade = FieldFade::default();
        fade.begin(FadeDirection::Out, 6, 1);
        assert_eq!(fade.brightness(), MasterBrightness::default());
        let mut seen = Vec::new();
        for _ in 0..6 {
            fade.update();
            seen.push(fade.brightness());
        }
        assert_eq!(
            seen,
            [down(2), down(5), down(7), down(10), down(13), down(16)]
        );
        assert!(!fade.is_finished());
        fade.update();
        assert!(fade.is_finished());
    }

    #[test]
    fn in_fade_matches_the_manager() {
        let mut fade = FieldFade::default();
        fade.begin(FadeDirection::In, 6, 1);
        assert_eq!(fade.brightness(), down(16));
        let mut seen = Vec::new();
        for _ in 0..6 {
            fade.update();
            seen.push(fade.brightness());
        }
        assert_eq!(
            seen,
            [
                down(13),
                down(10),
                down(8),
                down(5),
                down(2),
                MasterBrightness::default()
            ]
        );
    }

    fn sprite() -> PlayerSprite {
        PlayerSprite {
            strip: Arc::new(Texture {
                width: 32,
                height: 32 * 32,
                pixels: Vec::new(),
                format: crate::formats::TexFmt::Pltt256,
                color0_transparent: true,
                raw: None,
            }),
            frame_size: (32, 32),
            frame_count: 32,
            facing: Direction::South,
            frame: PlayerSprite::anim_start(Direction::South),
        }
    }

    #[test]
    fn walk_cycle_alternates_legs_as_the_oracle_shows() {
        // West: a turn (group 5, three ticks) then two normal steps
        // (group 3, eight ticks each, the END tick's callback advancing
        // one more): textures 9, 10, 10, 10 … as measured on the oracle
        // (docs/field-system.md).
        let mut s = sprite();
        let mut obj = MapObject::create(0, 0, Direction::South, 0, 1);
        obj.set_facing_direction_direct(Direction::West);
        obj.anim_group = 5;
        let mut shown = Vec::new();
        s.update(&obj, false);
        shown.push(s.texture_index());
        s.update(&obj, false);
        shown.push(s.texture_index());
        obj.anim_group = 0;
        s.update(&obj, false);
        shown.push(s.texture_index());
        for step in 0..2 {
            obj.anim_group = 3;
            for tick in 0..8 {
                let ended = tick == 7;
                if ended {
                    obj.anim_group = 0;
                }
                s.update(&obj, ended);
                shown.push(s.texture_index());
            }
            let _ = step;
        }
        // hero.N is texture N-1: the turn 10, 11, 11 | the first step
        // 11 11 11 12 12 12 12 then the END tick's wrap and snap to 9 |
        // the second step 9 9 9 10 10 10 10 then 11 — the oracle's
        // frames 4906-4941 (hero.9 and hero.11 are the same image).
        assert_eq!(
            shown,
            [
                9, 10, 10, 10, 10, 10, 11, 11, 11, 11, 8, 8, 8, 8, 9, 9, 9, 9, 10
            ]
        );
    }

    #[test]
    fn transition_schedule_matches_the_measured_stairs() {
        let t = Transition {
            kind: TransitionKind::Stairs,
            destination: Location::PLAYER_ROOM,
            tick: 0,
            stage: TransitionStage::Exit,
            loaded: false,
        };
        assert_eq!(t.fade_out_tick(), 21);
        assert_eq!(t.fade_in_tick(), 65);
        assert_eq!(t.end_tick(), 82);
        let e = Transition {
            kind: TransitionKind::Entrance,
            ..t
        };
        assert_eq!(e.fade_out_tick(), 3);
    }
}

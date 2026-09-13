//! The field camera — pret `src/camera.c` (C) and the field's camera
//! presets (`asm/overlay_01_021EABA8.s:465` `ov01_02206478`, 17 × 0x24
//! bytes, read by `FieldCamera_Create` at `:25-58`).
//!
//! The original keeps everything in the SDK's fixed point: angles are
//! 16-bit units (`0x10000` = 360°) looked up in `FX_SinCosTable_`
//! (`lib/include/nitro/fx/fx_trig.h:20-26`: 4096 `(sin, cos)` pairs of
//! fx16, indexed by `angle >> 4`), distances are fx32 (12 fractional
//! bits), and `FX_Mul`/`FX_Div` round half up as the ARM and the
//! hardware divider do.
//! The matrices and viewport path reproduce that arithmetic in signed
//! fixed point (`MTX_LookAt`, `MTX_PerspectiveW`, `MTX_OrthoW`,
//! `lib/NitroSDK/asm/fx_mtx4[34].s`). The SDK sine table is materialized
//! by `build.rs`; field rendering itself performs no floating-point work.
//!
//! Conventions, as the SDK's: row vectors (`v' = v · M`), camera space
//! has +x right, +y up, and the view looking down −z; clip space
//! divides by `w`, and the viewport `G3_ViewPort(0, 0, 255, 191)`
//! (`src/gf_3d_vramman.c:61`) maps NDC x ∈ [−1, 1] to 0..256 and NDC
//! y ∈ [−1, 1] to screen rows 192..0 (row 0 is the top).

/// One fixed-point unit: fx32 has twelve fractional bits.
pub const FX32_ONE: i32 = 1 << 12;

/// The SDK's `FX32_CONST(1.33333333)` aspect ratio (`camera.c:40`):
/// the C cast truncates `1.33333333 · 4096 = 5461.33` to 5461.
pub const ASPECT_FX32: i32 = 5461;

/// The field's billboard depth bias in world units — `FieldSystem.unk11C`
/// as `ov01_021E6220` (`src/field/fieldmap.c:580-613`) reads it: the
/// projection's `_32` gains `_22 · (8 · cos(−angle.x))` while the
/// field effects and billboard lists draw, so a sprite anchored at its
/// feet wins the depth test against the ground it stands on.
pub const BILLBOARD_BIAS_UNITS: i32 = 8;

/// The SDK's `FX_SinCosTable_`: entry `i` is `(sin, cos)` of
/// `i · 2π / 4096` in fx16 — `lib/NitroSDK/asm/fx_sincos.s`, which is
/// exactly `round(sin · 4096)` for all 4096 entries (verified against
/// the vendored table; the SHA-1 test below pins the regeneration).
fn sin_cos_table() -> &'static [[i16; 2]; 4096] {
    static TABLE: [[i16; 2]; 4096] = include!(concat!(env!("OUT_DIR"), "/fx_sincos.rs"));
    &TABLE
}

/// `FX_SinIdx(angle)`: the sine of a 16-bit angle as fx16/fx32
/// (`fx_trig.h:20`, `FX_SinCosTable_[(idx >> 4) << 1]`).
#[must_use]
pub fn sin_idx(angle: u16) -> i32 {
    i32::from(sin_cos_table()[usize::from(angle >> 4)][0])
}

/// `FX_CosIdx(angle)`: the cosine of a 16-bit angle as fx16/fx32
/// (`fx_trig.h:24`, `FX_SinCosTable_[((idx >> 4) << 1) + 1]`).
#[must_use]
pub fn cos_idx(angle: u16) -> i32 {
    i32::from(sin_cos_table()[usize::from(angle >> 4)][1])
}

/// `FX_Mul(a, b)`: fx32 × fx32 with the SDK's round-half-up,
/// `(a · b + 0x800) >> 12` in a 64-bit intermediate.
#[must_use]
pub fn fx_mul(a: i64, b: i64) -> i64 {
    (a * b + 0x800) >> 12
}

/// `FX_Div(a, b)`: fx32 ÷ fx32 through the hardware divider in its
/// 64/32 mode with 20 guard bits, rounded half up —
/// `lib/NitroSDK/asm/fx_cp.s` `FX_DivAsync` writes `DIVCNT = 1`,
/// `NUMER = a << 32`, `DENOM = b`, and `FX_GetDivResult` returns
/// `((a << 32) / b + 0x80000) >> 20` (`adds r2, r1, #0x80000; adc r1,
/// r0, #0; mov r0, r2, lsr #0x14; orr r0, r0, r1, lsl #12`). The
/// divider truncates the quotient toward zero; the `+ 0x80000`
/// (half of the 20 dropped bits) then rounds it to the nearest fx32,
/// halves up.
///
/// # Panics
/// Panics on a zero divisor, as the divider would flag.
#[must_use]
pub fn fx_div(a: i64, b: i64) -> i64 {
    assert!(b != 0, "FX_Div by zero");
    let quotient = (i128::from(a) << 32) / i128::from(b);
    i64::try_from((quotient + 0x8_0000) >> 20).expect("FX_Div result fits an i64")
}

/// `CameraParam.perspectiveType` (`include/camera.h:14-15`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Projection {
    /// `NNS_G3dGlbPerspective` from the half-angle's sine and cosine.
    Perspective,
    /// `NNS_G3dGlbOrtho` with extents `tan(half-angle) · distance`.
    Orthographic,
}

/// One row of the field camera table (`ov01_02206478`, 0x24 bytes):
/// the gfx-side shape of pret's preset. The core crate's own preset
/// type converts into this; [`CameraPreset::from_table_entry`] reads
/// the ROM row directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CameraPreset {
    /// Target-to-camera distance, fx32 (bytes 0..4).
    pub distance: i32,
    /// Camera angle about x, y, z in 16-bit units (bytes 4..10; a pad
    /// u16 follows). `x` pitches the camera above the target — the
    /// field's presets carry a negative pitch (`0xDC82` ≈ −50°).
    pub angle: [u16; 3],
    /// Perspective or orthographic (byte 12, read as u8 at `:36-39`).
    pub projection: Projection,
    /// `perspectiveAngle`: the vertical field of view's **half** angle
    /// in 16-bit units (bytes 14..16). `Camera_InitInternal` takes its
    /// sine and cosine (`camera.c:38-39`) and `MTX_PerspectiveW` reads
    /// them as `cot(fovy/2) = cos/sin`, gluPerspective's convention.
    pub fovy: u16,
    /// Near clipping plane, fx32 (bytes 16..20).
    pub near: i32,
    /// Far clipping plane, fx32 (bytes 20..24).
    pub far: i32,
    /// `Camera_OffsetLookAtPosAndTarget` offset, fx32 (bytes 24..36):
    /// added to both the camera and the target after creation.
    pub look_at_offset: [i32; 3],
}

impl CameraPreset {
    /// The size of one table row.
    pub const ENTRY_SIZE: usize = 0x24;
    /// The number of presets (`FieldCamera_Create` asserts `< 0x11`).
    pub const COUNT: usize = 17;

    /// Preset 0 — the outdoor default: perspective, distance
    /// `0x29AEC1` (≈ 666.9), pitch `0xDD62`, half-angle `0x5C1`
    /// (≈ 8.1°), near 150, far 1200, no offset.
    pub const OUTDOOR: Self = Self {
        distance: 0x0029_AEC1,
        angle: [0xDD62, 0, 0],
        projection: Projection::Perspective,
        fovy: 0x05C1,
        near: 0x0009_6000,
        far: 0x004B_0000,
        look_at_offset: [0, 0, 0],
    };

    /// Preset 4 — indoor rooms (the player's bedroom): orthographic,
    /// distance `0x61B89B` (≈ 1563.5), pitch `0xDC82`, half-angle
    /// `0x281` (≈ 3.5°), near 150, far 1736, no offset.
    pub const INDOOR: Self = Self {
        distance: 0x0061_B89B,
        angle: [0xDC82, 0, 0],
        projection: Projection::Orthographic,
        fovy: 0x0281,
        near: 0x0009_6000,
        far: 0x006C_7000,
        look_at_offset: [0, 0, 0],
    };

    /// Reads one 0x24-byte row of the ROM's camera table.
    #[must_use]
    pub fn from_table_entry(row: &[u8; Self::ENTRY_SIZE]) -> Self {
        let i32_at = |p: usize| i32::from_le_bytes([row[p], row[p + 1], row[p + 2], row[p + 3]]);
        let u16_at = |p: usize| u16::from_le_bytes([row[p], row[p + 1]]);
        Self {
            distance: i32_at(0),
            angle: [u16_at(4), u16_at(6), u16_at(8)],
            projection: if row[12] == 0 {
                Projection::Perspective
            } else {
                Projection::Orthographic
            },
            fovy: u16_at(14),
            near: i32_at(16),
            far: i32_at(20),
            look_at_offset: [i32_at(24), i32_at(28), i32_at(32)],
        }
    }
}

/// A signed 20.12 matrix used by the DS-compatible field path.
type FxMtx44 = [[i64; 4]; 4];

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct ClipPosition {
    pub x: i64,
    pub y: i64,
    pub z: i64,
    pub w: i64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct ScreenPosition {
    pub x: i32,
    pub y: i32,
    pub depth: u32,
    pub w: i64,
}

/// The resolved field camera: the view (`MTX_LookAt`) and projection
/// matrices for one preset and target, plus the billboard variant of
/// the projection with the field's depth bias folded in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Camera {
    position: [i64; 3],
    target: [i64; 3],
    billboard_bias: i64,
    fixed_view: FxMtx44,
    fixed_projection: FxMtx44,
    fixed_billboard_projection: FxMtx44,
    model_clip: Option<FxMtx44>,
}

impl Camera {
    /// `Camera_Init_FromTargetDistanceAndAngle` +
    /// `Camera_SetPerspectiveClippingPlane` +
    /// `Camera_OffsetLookAtPosAndTarget`, as `FieldCamera_Create` runs
    /// them (`asm/overlay_01_021EABA8.s:33-58`), for `target` in fx32
    /// world coordinates — the player's position vector (tile centre,
    /// ground height).
    #[must_use]
    pub fn from_preset(preset: &CameraPreset, target: [i32; 3]) -> Self {
        let [ax, ay, _] = preset.angle;
        let distance = i64::from(preset.distance);
        // Camera_CalcLookAtPosFromTargetAndAngle (camera.c:20-26):
        // note the x/z terms cosine the *positive* angle while y sines
        // the negated one — the table is not symmetric under rounding,
        // so both lookups are kept as written.
        let neg_x = ax.wrapping_neg();
        let cam_x = fx_mul(
            fx_mul(i64::from(sin_idx(ay)), distance),
            i64::from(cos_idx(ax)),
        );
        let cam_z = fx_mul(
            fx_mul(i64::from(cos_idx(ay)), distance),
            i64::from(cos_idx(ax)),
        );
        let cam_y = fx_mul(i64::from(sin_idx(neg_x)), distance);
        let offset = preset.look_at_offset.map(i64::from);
        let target_fx = [
            i64::from(target[0]) + offset[0],
            i64::from(target[1]) + offset[1],
            i64::from(target[2]) + offset[2],
        ];
        let position_fx = [
            target_fx[0] + cam_x,
            target_fx[1] + cam_y,
            target_fx[2] + cam_z,
        ];
        let fixed_target = target_fx;
        let fixed_position = position_fx;
        let fixed_view = look_at_fx(fixed_position, fixed_target);

        // Camera_ApplyPerspectiveType (camera.c:266-278).
        let fovy_sin = i64::from(sin_idx(preset.fovy));
        let fovy_cos = i64::from(cos_idx(preset.fovy));
        let fixed_projection = match preset.projection {
            Projection::Perspective => perspective_fx(
                fovy_sin,
                fovy_cos,
                i64::from(ASPECT_FX32),
                i64::from(preset.near),
                i64::from(preset.far),
            ),
            Projection::Orthographic => {
                let y = fx_mul(fx_div(fovy_sin, fovy_cos), distance);
                let x = fx_mul(y, i64::from(ASPECT_FX32));
                ortho_fx(y, -y, -x, x, i64::from(preset.near), i64::from(preset.far))
            }
        };

        // fieldmap.c:590-596: (unk11C << 12) * FX_CosIdx(-angle.x),
        // rounded to fx32, times _22, rounded, added to _32.
        let bias_fx =
            ((i64::from(BILLBOARD_BIAS_UNITS) << 12) * i64::from(cos_idx(neg_x)) + 0x800) >> 12;
        let mut fixed_billboard_projection = fixed_projection;
        fixed_billboard_projection[3][2] += fx_mul(fixed_projection[2][2], bias_fx);
        Self {
            position: position_fx,
            target: target_fx,
            billboard_bias: bias_fx,
            fixed_view,
            fixed_projection,
            fixed_billboard_projection,
            model_clip: None,
        }
    }

    /// Projects one fx32 world position with the DS integer viewport
    /// divider and returns integer screen coordinates and 24-bit depth.
    #[must_use]
    pub fn project_fixed(&self, world: [i32; 3]) -> Option<[i32; 3]> {
        let screen = viewport_fx(self.to_clip_fixed(world))?;
        Some([screen.x, screen.y, screen.depth as i32])
    }

    /// Transforms one fx32 world position to GX clip coordinates.
    ///
    /// This diagnostic form is used by the raw-oracle geometry gate;
    /// ordinary callers normally want [`Self::project_fixed`].
    #[must_use]
    pub fn clip_position_fixed(&self, world: [i32; 3]) -> [i64; 4] {
        let clip = self.to_clip_fixed(world);
        [clip.x, clip.y, clip.z, clip.w]
    }

    /// Camera position in fx32 world coordinates.
    #[must_use]
    pub fn position_fixed(&self) -> [i64; 3] {
        self.position
    }

    /// Look-at target in fx32 world coordinates.
    #[must_use]
    pub fn target_fixed(&self) -> [i64; 3] {
        self.target
    }

    /// Billboard depth bias in fx32 world units.
    #[must_use]
    pub fn billboard_bias_fixed(&self) -> i64 {
        self.billboard_bias
    }

    pub(crate) fn to_camera_fixed(&self, world: [i32; 3]) -> [i64; 3] {
        let point = world.map(i64::from);
        let result = mul_point_fx(&self.fixed_view, point);
        [result.x, result.y, result.z]
    }

    pub(crate) fn to_camera_normal(&self, vector: [i16; 3]) -> [i32; 3] {
        std::array::from_fn(|column| {
            let sum = (0..3)
                .map(|row| i64::from(vector[row]) * self.fixed_view[row][column])
                .sum::<i64>() as i32;
            sum.wrapping_shl(9) >> 21
        })
    }

    pub(crate) fn to_camera_light(&self, vector: [i16; 3]) -> ([i32; 3], i32) {
        let sums: [i32; 3] = std::array::from_fn(|column| {
            (0..3)
                .map(|row| i64::from(vector[row]) * self.fixed_view[row][column])
                .sum::<i64>() as i32
        });
        let direction = sums.map(|sum| ((-(sum >> 12)) << 21) >> 21);
        let denominator = 512 - (sums[2].wrapping_shl(9) >> 21);
        (direction, denominator)
    }

    pub(crate) fn camera_to_clip_fixed(&self, point: [i64; 3], billboard: bool) -> ClipPosition {
        let matrix = if billboard {
            &self.fixed_billboard_projection
        } else {
            &self.fixed_projection
        };
        mul_point_fx(matrix, point)
    }

    pub(crate) fn to_clip_fixed(&self, world: [i32; 3]) -> ClipPosition {
        if let Some(matrix) = &self.model_clip {
            return mul_point_fx(matrix, world.map(i64::from));
        }
        self.camera_to_clip_fixed(self.to_camera_fixed(world), false)
    }

    pub(crate) fn with_model(&self, transform: &apricorn_core::field::model::GxTransform) -> Self {
        let mut model = [[0i64; 4]; 4];
        let rotation = transform.matrix();
        for row in 0..3 {
            for column in 0..3 {
                model[row][column] = i64::from(rotation[column][row]);
            }
        }
        model[3][..3].copy_from_slice(&transform.translation().map(i64::from));
        model[3][3] = i64::from(FX32_ONE);
        let mut camera = self.clone();
        let position = multiply_matrix(&model, &self.fixed_view);
        camera.model_clip = Some(multiply_matrix(&position, &self.fixed_projection));
        camera
    }

    pub(crate) fn screen_fixed(clip: ClipPosition) -> Option<ScreenPosition> {
        viewport_fx(clip)
    }
}

fn multiply_matrix(a: &FxMtx44, b: &FxMtx44) -> FxMtx44 {
    std::array::from_fn(|row| {
        std::array::from_fn(|column| {
            let sum: i128 = (0..4)
                .map(|k| i128::from(a[row][k]) * i128::from(b[k][column]))
                .sum();
            i64::from((sum >> 12) as i32)
        })
    })
}

fn fx_from_ratio(numerator: i64, denominator: i64) -> i64 {
    fx_div(numerator, denominator)
}

/// Multiplies by the divider unit's fx64c reciprocal, preserving the
/// intermediate precision used by `MTX_OrthoW` before its final fx32
/// round. This is observably different from issuing a fresh division
/// for each matrix element.
fn fx_times_reciprocal(numerator: i64, denominator: i64) -> i64 {
    assert!(denominator != 0, "fixed reciprocal by zero");
    let reciprocal = (i128::from(FX32_ONE) << 32) / i128::from(denominator);
    ((i128::from(numerator) * reciprocal + 0x8000_0000) >> 32) as i64
}

fn dot_fx(a: [i64; 3], b: [i64; 3]) -> i64 {
    (a[0] * b[0] + a[1] * b[1] + a[2] * b[2] + 0x800) >> 12
}

fn cross_fx(a: [i64; 3], b: [i64; 3]) -> [i64; 3] {
    [
        (a[1] * b[2] - a[2] * b[1] + 0x800) >> 12,
        (a[2] * b[0] - a[0] * b[2] + 0x800) >> 12,
        (a[0] * b[1] - a[1] * b[0] + 0x800) >> 12,
    ]
}

fn integer_sqrt(value: u128) -> u128 {
    if value < 2 {
        return value;
    }
    let bits = 128 - value.leading_zeros() as usize;
    let mut x = 1u128 << bits.div_ceil(2);
    loop {
        let next = (x + value / x) >> 1;
        if next >= x {
            return x;
        }
        x = next;
    }
}

fn normalize_fx(vector: [i64; 3]) -> [i64; 3] {
    let length_squared = vector
        .iter()
        .map(|&value| i128::from(value) * i128::from(value))
        .sum::<i128>();
    let squared = length_squared as u128;
    assert!(squared != 0, "zero-length camera vector");
    // VEC_Normalize uses the DS divider and square-root units together:
    // floor(2^56 / |v|^2) * floor(sqrt(4|v|^2)), followed by the SDK's
    // signed round-half-up shift by 45.
    let reciprocal = (1u128 << 56) / squared;
    let twice_length = integer_sqrt(squared << 2);
    vector.map(|value| {
        let product = i128::from(value)
            * i128::try_from(reciprocal * twice_length).expect("normal reciprocal fits i128");
        ((product + (1i128 << 44)) >> 45) as i64
    })
}

fn look_at_fx(position: [i64; 3], target: [i64; 3]) -> FxMtx44 {
    let forward = normalize_fx([
        target[0] - position[0],
        target[1] - position[1],
        target[2] - position[2],
    ]);
    let side = normalize_fx(cross_fx(forward, [0, i64::from(FX32_ONE), 0]));
    let up = cross_fx(side, forward);
    [
        [side[0], up[0], -forward[0], 0],
        [side[1], up[1], -forward[1], 0],
        [side[2], up[2], -forward[2], 0],
        [
            -dot_fx(side, position),
            -dot_fx(up, position),
            dot_fx(forward, position),
            i64::from(FX32_ONE),
        ],
    ]
}

fn perspective_fx(fovy_sin: i64, fovy_cos: i64, aspect: i64, near: i64, far: i64) -> FxMtx44 {
    let cotangent = fx_from_ratio(fovy_cos, fovy_sin);
    let depth = far - near;
    [
        [fx_from_ratio(cotangent, aspect), 0, 0, 0],
        [0, cotangent, 0, 0],
        [
            0,
            0,
            -fx_from_ratio(far + near, depth),
            -i64::from(FX32_ONE),
        ],
        [0, 0, -fx_from_ratio(2 * fx_mul(far, near), depth), 0],
    ]
}

fn ortho_fx(top: i64, bottom: i64, left: i64, right: i64, near: i64, far: i64) -> FxMtx44 {
    let width = right - left;
    let height = top - bottom;
    let depth = far - near;
    let two = 2 * i64::from(FX32_ONE);
    [
        [fx_times_reciprocal(two, width), 0, 0, 0],
        [0, fx_times_reciprocal(two, height), 0, 0],
        [0, 0, -fx_times_reciprocal(two, depth), 0],
        [
            -fx_times_reciprocal(right + left, width),
            -fx_times_reciprocal(top + bottom, height),
            -fx_times_reciprocal(far + near, depth),
            i64::from(FX32_ONE),
        ],
    ]
}

fn mul_point_fx(matrix: &FxMtx44, point: [i64; 3]) -> ClipPosition {
    let component = |column: usize| {
        ((i128::from(point[0]) * i128::from(matrix[0][column])
            + i128::from(point[1]) * i128::from(matrix[1][column])
            + i128::from(point[2]) * i128::from(matrix[2][column]))
            >> 12) as i64
            + matrix[3][column]
    };
    ClipPosition {
        x: component(0),
        y: component(1),
        z: component(2),
        w: component(3),
    }
}

fn viewport_fx(clip: ClipPosition) -> Option<ScreenPosition> {
    if clip.w <= 0 {
        return None;
    }
    let mut x = clip.x + clip.w;
    let mut y = -clip.y + clip.w;
    let mut denominator = clip.w;
    // The geometry engine uses a 32-bit divider.  It gives up one bit
    // of numerator precision when W no longer fits in 16 bits.
    if denominator > 0xFFFF {
        x >>= 1;
        y >>= 1;
        denominator >>= 1;
    }
    denominator <<= 1;
    let screen_x = (i128::from(x) * 256 / i128::from(denominator)) as i32;
    let screen_y = (i128::from(y) * 192 / i128::from(denominator)) as i32;
    let depth = (((i128::from(clip.z) * 0x4000) / i128::from(clip.w) + 0x3FFF) * 0x200)
        .clamp(0, 0xFF_FFFF) as u32;
    Some(ScreenPosition {
        x: screen_x,
        y: screen_y,
        depth,
        w: clip.w,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha1::{Digest, Sha1};

    #[test]
    fn sin_cos_table_matches_the_sdk() {
        // Spot values read off lib/NitroSDK/asm/fx_sincos.s, and the
        // SHA-1 of all 8192 little-endian fx16 values as the file
        // lists them: the regeneration must reproduce the SDK bit for
        // bit, on every platform.
        assert_eq!((sin_idx(0), cos_idx(0)), (0, 0x1000));
        assert_eq!((sin_idx(0x10), cos_idx(0x10)), (6, 0x1000));
        assert_eq!((sin_idx(0x20), cos_idx(0x20)), (13, 0x1000));
        assert_eq!((sin_idx(0x4000), cos_idx(0x4000)), (0x1000, 0));
        assert_eq!((sin_idx(0x8000), cos_idx(0x8000)), (0, -0x1000));
        // The preset angles this module is actually asked for.
        assert_eq!((sin_idx(0xDC82), cos_idx(0xDC82)), (-3134, 2637));
        assert_eq!((sin_idx(0x237E), cos_idx(0x237E)), (3130, 2642));
        assert_eq!((sin_idx(0x5C1), cos_idx(0x5C1)), (576, 4055));
        assert_eq!((sin_idx(0x281), cos_idx(0x281)), (251, 4088));
        let mut hasher = Sha1::new();
        for entry in sin_cos_table().iter() {
            hasher.update(entry[0].to_le_bytes());
            hasher.update(entry[1].to_le_bytes());
        }
        assert_eq!(
            format!("{:x}", hasher.finalize()),
            "18b7e1baae69ac1be13a4edcd7805f2943c6847e"
        );
    }

    #[test]
    fn fx_mul_and_div_follow_the_sdk() {
        assert_eq!(fx_mul(FX32_ONE.into(), FX32_ONE.into()), FX32_ONE.into());
        // Round half up: 0.5 · 0.5 = 0.25 exactly; 1/4096 · 1/2 rounds up.
        assert_eq!(fx_mul(2048, 2048), 1024);
        assert_eq!(fx_mul(1, 2048), 1);
        assert_eq!(fx_div(FX32_ONE.into(), 2048), 2 * i64::from(FX32_ONE));
        // FX_Div rounds half up through 20 guard bits: 251/4088 in fx32
        // is 251.49 LSB, which rounds down to 251.
        assert_eq!(fx_div(251, 4088), 251);
        // Exactly half an LSB (1/8192 in fx32 = 0.5 LSB) rounds up;
        // one part in 8193 falls short of the half and rounds down.
        // Truncation, which the divider alone would give, yields 0 for
        // both.
        assert_eq!(fx_div(1, 8192), 1);
        assert_eq!(fx_div(1, 8193), 0);
        // The half-up rounding is toward +infinity on negatives too:
        // −0.5 LSB → 0, −1.5 LSB → −1 (the divider's quotient is
        // truncated toward zero before the guard bits are rounded).
        assert_eq!(fx_div(-1, 8192), 0);
        assert_eq!(fx_div(-3, 8192), -1);
        assert_eq!(fx_div(-1, 8193), 0);
        // A quotient just above a half rounds up: 3/8191 = 1.5002 LSB.
        assert_eq!(fx_div(3, 8191), 2);
    }

    #[test]
    fn preset_row_parses_the_documented_layout() {
        // Preset 4's row bytes, from ov01_02206478 + 4 · 0x24.
        let mut row = [0u8; 0x24];
        row[..4].copy_from_slice(&0x0061_B89Bu32.to_le_bytes());
        row[4..6].copy_from_slice(&0xDC82u16.to_le_bytes());
        row[12] = 1;
        row[14..16].copy_from_slice(&0x0281u16.to_le_bytes());
        row[16..20].copy_from_slice(&0x0009_6000u32.to_le_bytes());
        row[20..24].copy_from_slice(&0x006C_7000u32.to_le_bytes());
        assert_eq!(CameraPreset::from_table_entry(&row), CameraPreset::INDOOR);
    }

    /// The bedroom camera at the bedroom's player tile (6, 6): the
    /// numbers below are derived by hand from the C and the table.
    #[test]
    fn indoor_ortho_camera_pins_its_derivation() {
        let target = [(6 * 16 + 8) * FX32_ONE, 0, (6 * 16 + 8) * FX32_ONE];
        let cam = Camera::from_preset(&CameraPreset::INDOOR, target);

        assert_eq!(
            cam.position_fixed(),
            [target[0] as i64, 4_893_873, 4_549_033]
        );
        assert_eq!(cam.target_fixed(), target.map(i64::from));
        assert_eq!(cam.billboard_bias_fixed(), 21_136);
        assert_eq!(
            cam.fixed_projection,
            [
                [32, 0, 0, 0],
                [0, 43, 0, 0],
                [0, 0, -5, 0],
                [0, 0, -4871, 4096],
            ],
            "the SDK/GX orthographic matrix captured from the raw oracle"
        );
        assert_eq!(cam.fixed_view[0], [4096, 0, 0, 0]);
        assert_eq!(cam.fixed_view[1], [0, 2638, 3131, 0]);
        assert_eq!(cam.fixed_view[2], [0, -3131, 2638, 0]);
        assert_eq!(cam.fixed_view[3], [-425984, 325436, -6670670, 4096]);

        let centre = cam.project_fixed(target).unwrap();
        assert_eq!(&centre[..2], &[128, 96]);
        let east = cam
            .project_fixed([target[0] + 16 * FX32_ONE, 0, target[2]])
            .unwrap();
        assert!(east[0] > centre[0]);
        assert_eq!(east[1], centre[1]);

        let south = cam
            .project_fixed([target[0], 0, target[2] + 16 * FX32_ONE])
            .unwrap();
        assert!(south[1] > centre[1]);
        assert!(south[2] < centre[2]);

        let up = cam
            .project_fixed([target[0], 16 * FX32_ONE, target[2]])
            .unwrap();
        assert!(up[1] < centre[1]);

        let plain = cam.camera_to_clip_fixed([0, 0, -1000 * 4096], false);
        let biased = cam.camera_to_clip_fixed([0, 0, -1000 * 4096], true);
        assert!(biased.z < plain.z);
    }

    /// The outdoor perspective camera: pins the perspective terms and
    /// the ≈1:1 world-unit-to-pixel scale at the target depth.
    #[test]
    fn outdoor_perspective_camera_pins_its_derivation() {
        let target = [8 * FX32_ONE, 0, 8 * FX32_ONE];
        let cam = Camera::from_preset(&CameraPreset::OUTDOOR, target);
        assert_eq!(cam.fixed_projection[2][3], -FX32_ONE as i64);
        assert_eq!(cam.fixed_projection[3][3], 0);
        let centre = cam.project_fixed(target).unwrap();
        // SDK VEC_Normalize rounding and the viewport divider put the
        // perspective target on row 96.
        assert_eq!(&centre[..2], &[128, 96]);
        let camera_target = cam.to_camera_fixed(target);
        assert!(camera_target[2] < 0);

        let east = cam
            .project_fixed([target[0] + FX32_ONE, 0, target[2]])
            .unwrap();
        assert_eq!(east[0], centre[0] + 1);

        // Behind the eye projects to nothing.
        assert!(
            cam.project_fixed([target[0], 0, target[2] + 10_000 * FX32_ONE])
                .is_none()
        );
    }
}

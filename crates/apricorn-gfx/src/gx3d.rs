//! Deterministic Nintendo DS 3D raster primitives.
//!
//! The geometry stage hands this module viewport-quantized vertices.
//! Rasterization is integer-only: polygon edges are walked as scanline
//! spans and all varying values use fixed-point interpolation.  This is
//! intentionally separate from the desktop GPU, which only presents the
//! finished 256×192 image.

use apricorn_core::{
    field::{lighting::ModelLighting, model::Texture},
    formats::TexFmt,
};

/// Native width of the Nintendo DS 3D viewport.
pub const WIDTH: usize = 256;
/// Native height of the Nintendo DS 3D viewport.
pub const HEIGHT: usize = 192;
const FAR_DEPTH: u32 = 0xFF_FFFF;
const SLOPE_BITS: u32 = 18;
const SLOPE_ONE: i64 = 1 << SLOPE_BITS;
const SLOPE_HALF: i64 = SLOPE_ONE >> 1;

/// Depth value selected by `GX_SwapBuffers` for the submitted frame.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum DepthMode {
    /// Interpolate the 24-bit post-projection Z value.
    #[default]
    Z,
    /// Interpolate clipped W, reconstructed from the polygon-normalized value.
    W,
}

/// Registers which affect the final raster pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Gx3dFrame {
    /// `GX_SORTMODE_AUTO`; when false, only opaque/translucent partitioning applies.
    pub auto_sort: bool,
    /// Z- or W-buffering selected at buffer swap.
    pub depth_mode: DepthMode,
    /// Enables the DS edge-coverage resolve.
    pub antialiasing: bool,
    /// Enables the 3D core's five-bit alpha blending.
    pub alpha_blending: bool,
    /// Enables polygon-id edge marking.
    pub edge_marking: bool,
    /// Selects highlight shading instead of toon replacement for mode 2.
    pub highlight_shading: bool,
    /// Thirty-two BGR555 toon/highlight colours.
    pub toon_table: [u16; 32],
    /// Active GX field lights and global material registers.
    pub lighting: Option<ModelLighting>,
    /// The 128-entry specular response table used when enabled by a material.
    pub shininess_table: [u8; 128],
    /// Enables depth-table fog.
    pub fog: bool,
    /// Alpha values below this five-bit threshold are discarded.
    pub alpha_test: u8,
    /// Eight BGR555 edge colours, selected by polygon id / 8.
    pub edge_colors: [u16; 8],
    /// Thirty-two 7-bit fog-density entries.
    pub fog_table: [u8; 32],
    /// Fog depth offset in the renderer's 24-bit depth units.
    pub fog_offset: u32,
    /// Four-bit fog step shift from `DISP3DCNT`.
    pub fog_shift: u8,
    /// When set, fog changes alpha but leaves RGB untouched.
    pub fog_alpha_only: bool,
    /// Fog colour in BGR555.
    pub fog_color: u16,
    /// Five-bit fog alpha.
    pub fog_alpha: u8,
    /// Clear-plane colour in BGR555.
    pub clear_color: u16,
    /// Five-bit alpha of the clear plane. Zero keeps the field BG0 transparent.
    pub clear_alpha: u8,
    /// Native 24-bit clear depth.
    pub clear_depth: u32,
    /// Six-bit opaque polygon id stored on the clear plane.
    pub clear_polygon_id: u8,
    /// Fog-enable attribute of the clear plane.
    pub clear_fog: bool,
}

impl Default for Gx3dFrame {
    fn default() -> Self {
        Self {
            auto_sort: true,
            depth_mode: DepthMode::Z,
            antialiasing: false,
            alpha_blending: true,
            edge_marking: false,
            highlight_shading: false,
            toon_table: [0; 32],
            lighting: None,
            shininess_table: [0; 128],
            fog: false,
            alpha_test: 0,
            edge_colors: [0; 8],
            fog_table: [0; 32],
            fog_offset: 0,
            fog_shift: 0,
            fog_alpha_only: false,
            fog_color: 0,
            fog_alpha: 0,
            clear_color: 0,
            clear_alpha: 0,
            clear_depth: FAR_DEPTH,
            clear_polygon_id: 63,
            clear_fog: false,
        }
    }
}

/// One complete native-resolution 3D framebuffer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Gx3dBuffer {
    color: Box<[[u8; 4]]>,
    under_color: Box<[[u8; 4]]>,
    depth: Box<[u32]>,
    under_depth: Box<[u32]>,
    opaque_polygon: Box<[u8]>,
    under_opaque_polygon: Box<[u8]>,
    translucent_polygon: Box<[u8]>,
    under_translucent_polygon: Box<[u8]>,
    fogged: Box<[bool]>,
    under_fogged: Box<[bool]>,
    coverage: Box<[u8]>,
    under_coverage: Box<[u8]>,
    edge_flags: Box<[u8]>,
    under_edge_flags: Box<[u8]>,
    stencil: Box<[u8]>,
    previous_shadow_mask: bool,
}

impl Default for Gx3dBuffer {
    fn default() -> Self {
        Self::new()
    }
}

impl Gx3dBuffer {
    /// Allocates a transparent buffer at the native DS dimensions.
    #[must_use]
    pub fn new() -> Self {
        Self {
            color: vec![[0; 4]; WIDTH * HEIGHT].into_boxed_slice(),
            under_color: vec![[0; 4]; WIDTH * HEIGHT].into_boxed_slice(),
            depth: vec![FAR_DEPTH; WIDTH * HEIGHT].into_boxed_slice(),
            under_depth: vec![FAR_DEPTH; WIDTH * HEIGHT].into_boxed_slice(),
            opaque_polygon: vec![0xFF; WIDTH * HEIGHT].into_boxed_slice(),
            under_opaque_polygon: vec![0xFF; WIDTH * HEIGHT].into_boxed_slice(),
            translucent_polygon: vec![0xFF; WIDTH * HEIGHT].into_boxed_slice(),
            under_translucent_polygon: vec![0xFF; WIDTH * HEIGHT].into_boxed_slice(),
            fogged: vec![false; WIDTH * HEIGHT].into_boxed_slice(),
            under_fogged: vec![false; WIDTH * HEIGHT].into_boxed_slice(),
            coverage: vec![0xFF; WIDTH * HEIGHT].into_boxed_slice(),
            under_coverage: vec![0xFF; WIDTH * HEIGHT].into_boxed_slice(),
            edge_flags: vec![0; WIDTH * HEIGHT].into_boxed_slice(),
            under_edge_flags: vec![0; WIDTH * HEIGHT].into_boxed_slice(),
            stencil: vec![0; WIDTH * HEIGHT].into_boxed_slice(),
            previous_shadow_mask: false,
        }
    }

    /// Resets colour, depth and polygon attributes for a new frame.
    pub fn clear(&mut self) {
        self.color.fill([0; 4]);
        self.under_color.fill([0; 4]);
        self.depth.fill(FAR_DEPTH);
        self.under_depth.fill(FAR_DEPTH);
        self.opaque_polygon.fill(0xFF);
        self.under_opaque_polygon.fill(0xFF);
        self.translucent_polygon.fill(0xFF);
        self.under_translucent_polygon.fill(0xFF);
        self.fogged.fill(false);
        self.under_fogged.fill(false);
        self.coverage.fill(0xFF);
        self.under_coverage.fill(0xFF);
        self.edge_flags.fill(0);
        self.under_edge_flags.fill(0);
        self.stencil.fill(0);
        self.previous_shadow_mask = false;
    }

    /// Resets the two hardware pixel layers from the frame's clear registers.
    pub fn clear_with_frame(&mut self, frame: &Gx3dFrame) {
        let mut color = bgr555(frame.clear_color);
        color[3] = expand_alpha5(frame.clear_alpha.min(31));
        self.color.fill(color);
        self.under_color.fill(color);
        self.depth.fill(frame.clear_depth.min(FAR_DEPTH));
        self.under_depth.fill(frame.clear_depth.min(FAR_DEPTH));
        self.opaque_polygon.fill(frame.clear_polygon_id & 0x3f);
        self.under_opaque_polygon
            .fill(frame.clear_polygon_id & 0x3f);
        self.translucent_polygon.fill(0xFF);
        self.under_translucent_polygon.fill(0xFF);
        self.fogged.fill(frame.clear_fog);
        self.under_fogged.fill(frame.clear_fog);
        self.coverage.fill(0xFF);
        self.under_coverage.fill(0xFF);
        self.edge_flags.fill(0);
        self.under_edge_flags.fill(0);
        self.stencil.fill(0);
        self.previous_shadow_mask = false;
    }

    /// Resolved RGBA8 pixels; alpha zero denotes the transparent 3D clear.
    #[must_use]
    pub fn colors(&self) -> &[[u8; 4]] {
        &self.color
    }

    /// The 24-bit depth plane.
    #[must_use]
    pub fn depths(&self) -> &[u32] {
        &self.depth
    }

    /// The six-bit polygon-id plane (`0xff` where clear).
    #[must_use]
    pub fn polygon_ids(&self) -> &[u8] {
        &self.opaque_polygon
    }

    /// The last translucent polygon id at each pixel (`0xff` where none).
    #[must_use]
    pub fn translucent_polygon_ids(&self) -> &[u8] {
        &self.translucent_polygon
    }

    /// Five-bit antialias coverage (`0xff` for a non-edge pixel).
    #[must_use]
    pub fn coverages(&self) -> &[u8] {
        &self.coverage
    }

    /// Packs the resolved top layer as the oracle's native R6/G6/B6/A5 words.
    #[must_use]
    pub fn native_colors(&self) -> Vec<u32> {
        self.color
            .iter()
            .map(|&pixel| pack_native_color(pixel))
            .collect()
    }

    /// Packs edge, coverage, fog and polygon-id state in the oracle layout.
    #[must_use]
    pub fn native_attributes(&self) -> Vec<u32> {
        (0..self.color.len())
            .map(|index| {
                let mut value = u32::from(self.edge_flags[index] & 0x0f);
                if self.edge_flags[index] != 0 {
                    let coverage = if self.coverage[index] == 0xff {
                        31
                    } else {
                        self.coverage[index].min(31)
                    };
                    value |= u32::from(coverage) << 8;
                }
                if self.fogged[index] {
                    value |= 1 << 15;
                }
                if self.translucent_polygon[index] != 0xff {
                    value |= u32::from(self.translucent_polygon[index] & 0x7f) << 16;
                    value |= 1 << 22;
                }
                value | (u32::from(self.opaque_polygon[index] & 0x3f) << 24)
            })
            .collect()
    }

    /// Applies edge marking, fog, then coverage resolve in hardware order.
    pub fn resolve(&mut self, frame: &Gx3dFrame) {
        if frame.edge_marking {
            for y in 0..HEIGHT {
                for x in 0..WIDTH {
                    let index = y * WIDTH + x;
                    let id = self.opaque_polygon[index];
                    if self.edge_flags[index] == 0 {
                        continue;
                    }
                    let neighbour = |nx: isize, ny: isize| {
                        if nx < 0 || ny < 0 || nx >= WIDTH as isize || ny >= HEIGHT as isize {
                            (
                                frame.clear_polygon_id & 0x3f,
                                frame.clear_depth.min(FAR_DEPTH),
                            )
                        } else {
                            let neighbour = ny as usize * WIDTH + nx as usize;
                            (self.opaque_polygon[neighbour], self.depth[neighbour])
                        }
                    };
                    let edge = [
                        neighbour(x as isize - 1, y as isize),
                        neighbour(x as isize + 1, y as isize),
                        neighbour(x as isize, y as isize - 1),
                        neighbour(x as isize, y as isize + 1),
                    ]
                    .into_iter()
                    .any(|(neighbour_id, neighbour_depth)| {
                        neighbour_id != id && self.depth[index] < neighbour_depth
                    });
                    if edge {
                        let mut color = bgr555(frame.edge_colors[usize::from(id >> 3)]);
                        color[3] = self.color[index][3];
                        self.color[index] = color;
                        if self.coverage[index] != 0xFF {
                            self.coverage[index] = 0x10;
                        }
                    }
                }
            }
        }
        if frame.fog {
            let fog = bgr555(frame.fog_color);
            for index in 0..self.color.len() {
                fog_pixel(
                    &mut self.color[index],
                    self.depth[index],
                    self.fogged[index],
                    frame,
                    fog,
                );
                if self.coverage[index] != 0xFF {
                    fog_pixel(
                        &mut self.under_color[index],
                        self.under_depth[index],
                        self.under_fogged[index],
                        frame,
                        fog,
                    );
                }
            }
        }
        if frame.antialiasing {
            for index in 0..self.color.len() {
                let coverage = self.coverage[index];
                if coverage == 0xFF || coverage == 31 {
                    continue;
                }
                if coverage == 0 {
                    self.color[index] = self.under_color[index];
                    continue;
                }
                let weight = u16::from(coverage) + 1;
                let bottom = self.under_color[index];
                let mut top = self.color[index];
                if bottom[3] != 0 {
                    for channel in 0..3 {
                        let mixed = (u16::from(collapse_color6(top[channel])) * weight
                            + u16::from(collapse_color6(bottom[channel])) * (32 - weight))
                            >> 5;
                        top[channel] = expand_color6(mixed as u8);
                    }
                }
                let alpha = (u16::from(collapse_alpha5(top[3])) * weight
                    + u16::from(collapse_alpha5(bottom[3])) * (32 - weight))
                    >> 5;
                top[3] = expand_alpha5(alpha as u8);
                self.color[index] = top;
            }
        }
    }
}

fn fog_pixel(pixel: &mut [u8; 4], depth: u32, enabled: bool, frame: &Gx3dFrame, fog: [u8; 4]) {
    if !enabled || pixel[3] == 0 {
        return;
    }
    let density = u16::from(fog_density(frame, depth));
    if !frame.fog_alpha_only {
        for (channel, target) in pixel[..3].iter_mut().zip(fog) {
            let value = (u16::from(collapse_color6(*channel)) * (128 - density)
                + u16::from(collapse_color6(target)) * density)
                >> 7;
            *channel = expand_color6(value as u8);
        }
    }
    let alpha = (u16::from(collapse_alpha5(pixel[3])) * (128 - density)
        + u16::from(frame.fog_alpha.min(31)) * density)
        >> 7;
    pixel[3] = expand_alpha5(alpha as u8);
}

fn fog_density(frame: &Gx3dFrame, depth: u32) -> u8 {
    let shifted = depth
        .saturating_sub(frame.fog_offset)
        .wrapping_shr(2)
        .wrapping_shl(u32::from(frame.fog_shift & 0x0f));
    let (index, fraction) = if depth < frame.fog_offset {
        (0usize, 0u32)
    } else {
        let index = shifted >> 17;
        if index >= 32 {
            (32usize, 0)
        } else {
            (index as usize, shifted & 0x1ffff)
        }
    };
    let entry = |slot: usize| match slot {
        0 => frame.fog_table[0],
        1..=32 => frame.fog_table[slot - 1],
        _ => frame.fog_table[31],
    };
    let density = (u32::from(entry(index)) * (0x20000 - fraction)
        + u32::from(entry(index + 1)) * fraction)
        >> 17;
    if density >= 127 { 128 } else { density as u8 }
}

fn bgr555(value: u16) -> [u8; 4] {
    let expand = |component: u16| {
        let six = if component == 0 {
            0
        } else {
            ((component as u8) << 1) | 1
        };
        (six << 2) | (six >> 4)
    };
    [
        expand(value & 31),
        expand((value >> 5) & 31),
        expand((value >> 10) & 31),
        255,
    ]
}

fn expand_color6(value: u8) -> u8 {
    let value = value.min(63);
    (value << 2) | (value >> 4)
}

fn collapse_color6(value: u8) -> u8 {
    value >> 2
}

fn expand_alpha5(value: u8) -> u8 {
    (u16::from(value.min(31)) * 255 / 31) as u8
}

fn collapse_alpha5(value: u8) -> u8 {
    ((u16::from(value) * 31 + 127) / 255) as u8
}

fn pack_native_color(pixel: [u8; 4]) -> u32 {
    u32::from(collapse_color6(pixel[0]))
        | (u32::from(collapse_color6(pixel[1])) << 8)
        | (u32::from(collapse_color6(pixel[2])) << 16)
        | (u32::from(collapse_alpha5(pixel[3])) << 24)
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Projected {
    pub x: i32,
    pub y: i32,
    pub depth: u32,
    pub w: i64,
    /// Texture coordinates with four fractional bits.
    pub uv: [i64; 2],
    /// Nine-bit expanded vertex colours; raster output uses bits 3–8.
    pub color: [i64; 3],
    /// Signed screen winding before viewport quantization; zero for fixtures.
    pub winding: i8,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct Surface<'a> {
    pub texture: Option<&'a Texture>,
    pub flags: u32,
    pub alpha: u8,
    pub polygon_id: u8,
    pub fog: bool,
    pub depth_equal: bool,
    pub depth_write: bool,
    /// GX polygon mode (bits 4-5); mode 3 selects shadow polygons.
    pub mode: u8,
}

#[derive(Debug, Clone, Copy, Default)]
struct EdgeSpec {
    top: Projected,
    bottom: Projected,
    exact_x: i64,
    left: bool,
}

#[derive(Debug, Clone, Copy, Default)]
struct SpanPoint {
    x: i32,
    depth: i64,
    w: i64,
    uv: [i64; 2],
    color: [i64; 3],
    increment: i64,
    negative: bool,
    x_major: bool,
    bottom_x: i32,
    edge_length: i32,
    coverage_start: i64,
    coverage_increment: i64,
}

/// The DS does not interpolate `A/W` and `1/W` at host precision.
/// It first derives an 8-bit (X) or 9-bit (Y) perspective factor and
/// applies that factor to the original endpoint attributes. Equal,
/// suitably aligned W values take a separate exact linear path.
fn interpolate_attribute(
    a: i64,
    b: i64,
    position: i64,
    start: i64,
    end: i64,
    w0: i64,
    w1: i64,
    vertical: bool,
) -> i64 {
    let length = end - start;
    if length == 0 || a == b {
        return a;
    }
    let offset = position - start;
    let low_mask = if vertical { 0x7e } else { 0x7f };
    if w0 == w1 && w0 & low_mask == 0 {
        return if a < b {
            a + (i128::from(b - a) * i128::from(offset) / i128::from(length)) as i64
        } else {
            b + (i128::from(a - b) * i128::from(length - offset) / i128::from(length)) as i64
        };
    }

    let precision = if vertical { 9 } else { 8 };
    let (numerator_w, denominator_w0, denominator_w1) = if vertical {
        (w0 >> 1, (w0 + ((w0 & !w1) & 1)) >> 1, w1 >> 1)
    } else {
        (w0, w0, w1)
    };
    let numerator = i128::from(offset) * i128::from(numerator_w) << precision;
    let denominator = i128::from(offset) * i128::from(denominator_w0)
        + i128::from(length - offset) * i128::from(denominator_w1);
    let factor = if denominator == 0 {
        0
    } else {
        (numerator / denominator) as i64
    };
    if a < b {
        a + ((i128::from(b - a) * i128::from(factor)) >> precision) as i64
    } else {
        b + ((i128::from(a - b) * i128::from((1 << precision) - factor)) >> precision) as i64
    }
}

fn interpolate_depth(
    a: i64,
    b: i64,
    position: i64,
    start: i64,
    end: i64,
    w0: i64,
    w1: i64,
    vertical: bool,
    mode: DepthMode,
) -> i64 {
    if mode == DepthMode::W {
        return interpolate_attribute(a, b, position, start, end, w0, w1, vertical);
    }
    let length = end - start;
    if length == 0 || a == b {
        return a;
    }
    let offset = position - start;
    let reciprocal = (1i64 << 22) / length;
    let (base, mut displacement, factor) = if a < b {
        (a, b - a, offset)
    } else {
        (b, a - b, length - offset)
    };
    if vertical {
        let mut shift = 0;
        while displacement > 0x3ff {
            displacement >>= 1;
            shift += 1;
        }
        base + (((displacement * factor * reciprocal) >> 22) << shift)
    } else {
        displacement >>= 9;
        base + ((displacement * factor * reciprocal) >> 13)
    }
}

fn depth_passes(mode: DepthMode, equal: bool, stored: u32, candidate: u32) -> bool {
    if equal {
        stored.abs_diff(candidate) <= if mode == DepthMode::W { 0xff } else { 0x200 }
    } else {
        candidate < stored
    }
}

fn plot_translucent(
    buffer: &mut Gx3dBuffer,
    pixel: usize,
    under: bool,
    native: [u8; 4],
    surface: &Surface<'_>,
    depth: Option<u32>,
    blending: bool,
) -> bool {
    let polygon_id = surface.polygon_id & 0x3f;
    let (color, stored_depth, opaque_id, translucent_id, fogged) = if under {
        (
            &mut buffer.under_color[pixel],
            &mut buffer.under_depth[pixel],
            buffer.under_opaque_polygon[pixel],
            &mut buffer.under_translucent_polygon[pixel],
            &mut buffer.under_fogged[pixel],
        )
    } else {
        (
            &mut buffer.color[pixel],
            &mut buffer.depth[pixel],
            buffer.opaque_polygon[pixel],
            &mut buffer.translucent_polygon[pixel],
            &mut buffer.fogged[pixel],
        )
    };
    let shadow = surface.mode == 3 && polygon_id != 0;
    if if shadow {
        (*translucent_id != 0xff && *translucent_id == polygon_id)
            || (*translucent_id == 0xff && opaque_id == polygon_id)
    } else {
        *translucent_id == polygon_id
    } {
        return false;
    }
    blend(color, native, blending);
    if let Some(depth) = depth {
        *stored_depth = depth;
    }
    *translucent_id = polygon_id;
    *fogged &= surface.fog;
    true
}

fn normalized_w(vertices: &mut [Projected], mode: DepthMode) {
    let mut magnitude_bits = 0u32;
    for vertex in vertices.iter() {
        let w = (vertex.w as u64) & 0x00ff_ffff;
        let bits = if w == 0 { 0 } else { 64 - w.leading_zeros() };
        magnitude_bits = magnitude_bits.max(bits.div_ceil(4) * 4);
    }
    for vertex in vertices {
        let w = (vertex.w as u64) & 0x00ff_ffff;
        let (normalized, reconstructed) = if magnitude_bits < 16 {
            let shift = 16 - magnitude_bits;
            let normalized = w << shift;
            (normalized, normalized >> shift)
        } else {
            let shift = magnitude_bits - 16;
            let normalized = w >> shift;
            (normalized, normalized << shift)
        };
        vertex.w = normalized as i64;
        if mode == DepthMode::W {
            vertex.depth = reconstructed.min(u64::from(FAR_DEPTH)) as u32;
        }
    }
}

fn edge_point(
    edge: EdgeSpec,
    y: i32,
    right: bool,
    depth_mode: DepthMode,
    swapped: bool,
) -> SpanPoint {
    let dx = i64::from(edge.bottom.x - edge.top.x);
    let dy = i64::from(edge.bottom.y - edge.top.y);
    // GX stores a nonvertical edge's maximum X exclusively before it
    // evaluates `xmax + 1 - xmin`, so the resulting length is `abs(dx)`.
    // A vertical edge is the sole one-pixel special case.
    let x_length = if dx == 0 { 1 } else { dx.abs() };
    let increment = if dy == x_length && x_length != 1 {
        SLOPE_ONE
    } else {
        dx.abs() * (SLOPE_ONE / dy)
    };
    let negative = dx < 0;
    let x_major = increment > SLOPE_ONE;
    let mut distance = match (right, x_major, negative, increment != 0) {
        (true, true, true, _) => SLOPE_ONE + SLOPE_HALF,
        (true, true, false, _) => increment - SLOPE_HALF,
        (true, false, true, true) => SLOPE_ONE,
        (false, true, true, _) => increment + SLOPE_HALF,
        (false, true, false, _) => SLOPE_HALF,
        (false, false, true, true) => SLOPE_ONE,
        _ => 0,
    };
    distance += i64::from(y - edge.top.y) * increment;
    let (minimum, maximum) = if dx > 0 {
        (edge.top.x, edge.bottom.x - 1)
    } else if dx < 0 {
        (edge.bottom.x, edge.top.x - 1)
    } else {
        (edge.top.x, edge.top.x)
    };
    let stepped = if negative {
        i64::from(edge.top.x) - (distance >> SLOPE_BITS)
    } else {
        i64::from(edge.top.x) + (distance >> SLOPE_BITS)
    };
    let x = stepped.clamp(i64::from(minimum), i64::from(maximum)) as i32;
    let mut edge_length = if x_major {
        let length = if right != negative {
            (distance >> SLOPE_BITS) - ((distance - increment) >> SLOPE_BITS)
        } else {
            ((distance + increment) >> SLOPE_BITS) - (distance >> SLOPE_BITS)
        };
        length.max(1) as i32
    } else {
        1
    };
    let (coverage_start, coverage_increment) = if x_major {
        let mut start_x = distance >> SLOPE_BITS;
        if negative {
            start_x = x_length - start_x;
        }
        if right {
            start_x = start_x - i64::from(edge_length) + 1;
        }
        (
            ((((start_x << 10) + 0x1FF) * dy) / x_length) & 0x3FF,
            (dy << 10) / x_length,
        )
    } else if increment == 0 {
        (if swapped { 0 } else { 31 }, 0)
    } else {
        let mut coverage = ((distance >> 9) + (increment >> 10)) >> 4;
        if (coverage >> 5) != (distance >> SLOPE_BITS) {
            coverage = 31;
        }
        coverage &= 31;
        if if swapped {
            right != negative
        } else {
            right == negative
        } {
            coverage = 31 - coverage;
        }
        (coverage, 0)
    };
    if swapped && x_major {
        edge_length = 1;
    }

    let interpolation_offset = i32::from(increment >= SLOPE_ONE && (right != negative));
    let start = i64::from(edge.top.y - interpolation_offset);
    let end = i64::from(edge.bottom.y - interpolation_offset);
    let position = i64::from(y);
    let attr =
        |a, b| interpolate_attribute(a, b, position, start, end, edge.top.w, edge.bottom.w, true);
    SpanPoint {
        x,
        depth: interpolate_depth(
            i64::from(edge.top.depth),
            i64::from(edge.bottom.depth),
            position,
            start,
            end,
            edge.top.w,
            edge.bottom.w,
            true,
            depth_mode,
        ),
        w: attr(edge.top.w, edge.bottom.w),
        uv: [
            attr(edge.top.uv[0], edge.bottom.uv[0]),
            attr(edge.top.uv[1], edge.bottom.uv[1]),
        ],
        color: [
            attr(edge.top.color[0], edge.bottom.color[0]),
            attr(edge.top.color[1], edge.bottom.color[1]),
            attr(edge.top.color[2], edge.bottom.color[2]),
        ],
        increment,
        negative,
        x_major,
        bottom_x: edge.bottom.x,
        edge_length,
        coverage_start,
        coverage_increment,
    }
}

#[cfg(test)]
pub(crate) fn draw_triangle(
    vertices: [Projected; 3],
    surface: &Surface<'_>,
    _write_depth: bool,
    frame: &Gx3dFrame,
    buffer: &mut Gx3dBuffer,
) {
    draw_polygon(&vertices, surface, _write_depth, false, frame, buffer);
}

#[cfg(test)]
pub(crate) fn draw_quad(
    vertices: [Projected; 4],
    surface: &Surface<'_>,
    _write_depth: bool,
    frame: &Gx3dFrame,
    buffer: &mut Gx3dBuffer,
) {
    draw_polygon(&vertices, surface, _write_depth, false, frame, buffer);
}

pub(crate) fn draw_clipped_polygon(
    vertices: &[Projected],
    surface: &Surface<'_>,
    _write_depth: bool,
    fill_all_edges: bool,
    frame: &Gx3dFrame,
    buffer: &mut Gx3dBuffer,
) {
    draw_polygon(
        vertices,
        surface,
        _write_depth,
        fill_all_edges,
        frame,
        buffer,
    );
}

fn draw_polygon(
    vertices: &[Projected],
    surface: &Surface<'_>,
    _write_depth: bool,
    fill_all_edges: bool,
    frame: &Gx3dFrame,
    buffer: &mut Gx3dBuffer,
) {
    if vertices.len() < 3 || vertices.iter().any(|vertex| vertex.w <= 0) {
        return;
    }
    let is_shadow_mask = surface.mode == 3 && surface.polygon_id == 0;
    if is_shadow_mask {
        if !buffer.previous_shadow_mask {
            buffer.stencil.fill(0);
        }
        buffer.previous_shadow_mask = true;
    } else {
        buffer.previous_shadow_mask = false;
    }
    let mut polygon = vertices.to_vec();
    normalized_w(&mut polygon, frame.depth_mode);
    let vertices = polygon.as_slice();

    let y_min = vertices.iter().map(|vertex| vertex.y).min().unwrap().max(0);
    let raw_y_max = vertices.iter().map(|vertex| vertex.y).max().unwrap();
    let horizontal = vertices.iter().all(|vertex| vertex.y == vertices[0].y);
    let y_max = (raw_y_max + i32::from(horizontal)).min(HEIGHT as i32);
    if y_min >= y_max {
        return;
    }

    for y in y_min..y_max {
        let mut edges = [EdgeSpec::default(); 4];
        let mut edge_count = 0;
        if horizontal {
            let first = *vertices.iter().min_by_key(|vertex| vertex.x).unwrap();
            let last = *vertices.iter().max_by_key(|vertex| vertex.x).unwrap();
            for (index, vertex) in [first, last].into_iter().enumerate() {
                edges[index] = EdgeSpec {
                    top: vertex,
                    bottom: Projected {
                        y: vertex.y + 1,
                        ..vertex
                    },
                    exact_x: i64::from(vertex.x) << 32,
                    left: index == 0,
                };
            }
            edge_count = 2;
        }
        for index in 0..vertices.len() {
            let a = vertices[index];
            let b = vertices[(index + 1) % vertices.len()];
            if a.y == b.y {
                continue;
            }
            let (top, bottom) = if a.y < b.y { (a, b) } else { (b, a) };
            if y < top.y || y >= bottom.y {
                continue;
            }
            let dy = i64::from(bottom.y - top.y);
            let exact_x = (i64::from(top.x) << 32)
                + ((i64::from(bottom.x - top.x) * i64::from(y - top.y)) << 32) / dy;
            edges[edge_count] = EdgeSpec {
                top,
                bottom,
                exact_x,
                left: (a.y < b.y) == (vertices[0].winding < 0),
            };
            edge_count += 1;
        }
        if edge_count < 2 {
            continue;
        }
        let edges = &mut edges[..edge_count];
        edges.sort_by(|a, b| {
            a.exact_x.cmp(&b.exact_x).then_with(|| {
                if vertices[0].winding != 0 {
                    return b.left.cmp(&a.left);
                }
                // At an apex both intersections are identical. Classify the
                // edges by their slope immediately below it, independently
                // of the polygon's vertex order.
                let a_slope = i64::from(a.bottom.x - a.top.x) * i64::from(b.bottom.y - b.top.y);
                let b_slope = i64::from(b.bottom.x - b.top.x) * i64::from(a.bottom.y - a.top.y);
                a_slope.cmp(&b_slope)
            })
        });
        let left_spec = edges[0];
        let right_spec = edges[edges.len() - 1];
        let mut left = edge_point(left_spec, y, false, frame.depth_mode, false);
        let mut right = edge_point(right_spec, y, true, frame.depth_mode, false);
        let mut x_start = left.x;
        let mut x_end = right.x;
        let mut swapped_edges = false;
        // A vertical right boundary belongs to the polygon on its
        // right, except for a zero-width line at the viewport edge.
        if right.increment == 0 && (left.increment != 0 || x_start != x_end) && x_end != 0 {
            x_end -= 1;
        }
        if x_start > x_end {
            swapped_edges = true;
            let old_start = x_start;
            let old_end = x_end;
            left = edge_point(right_spec, y, true, frame.depth_mode, true);
            right = edge_point(left_spec, y, false, frame.depth_mode, true);
            x_start = old_end;
            x_end = old_start;
        }
        // GX constructs the horizontal interpolator from the complete
        // edge span before its ownership rules skip unfilled edge
        // pixels. Changing these endpoints when an edge is omitted
        // shifts every texture/color sample on that scanline.
        let interpolation_start = i64::from(x_start);
        let interpolation_end = i64::from(x_end) + 1;
        let full_x_start = x_start;
        let full_x_end = x_end;
        // Opaque polygons use the GX edge-ownership rules.  This is
        // deliberately not a generic top-left rule: shallow negative
        // left edges and shallow positive right edges own their edge
        // pixels, while their opposite neighbours do not.  At the last
        // scanline both X-major edges are retained when they converge on
        // different endpoints so adjacent polygons cannot expose a gap.
        if surface.alpha == 31 && !fill_all_edges && !frame.antialiasing && !frame.edge_marking {
            let bottom_row = y == y_max - 1;
            let distinct_bottoms = left.bottom_x != right.bottom_x;
            let left_filled = left.negative
                || !left.x_major
                || (bottom_row && left.x_major && distinct_bottoms)
                || (left.increment == right.increment && x_start + left.edge_length == x_end + 1);
            let right_filled = (!right.negative && right.x_major)
                || if swapped_edges {
                    !(right.negative && right.x_major) && left.increment == 0
                } else {
                    right.increment == 0
                }
                || (bottom_row && right.x_major && distinct_bottoms);
            if !left_filled {
                x_start += left.edge_length;
            }
            if !right_filled {
                x_end -= right.edge_length;
            }
        }
        if x_end < 0 || x_start >= WIDTH as i32 {
            continue;
        }
        let attr = |a, b, x| {
            interpolate_attribute(
                a,
                b,
                x,
                interpolation_start,
                interpolation_end,
                left.w,
                right.w,
                false,
            )
        };
        let mut left_coverage = if left.coverage_start == 0x3ff {
            0
        } else {
            left.coverage_start
        };
        let mut right_coverage = if right.coverage_start == 0x3ff {
            0
        } else {
            right.coverage_start
        };
        for x in x_start.max(0)..=x_end.min(WIDTH as i32 - 1) {
            let position = i64::from(x);
            let left_edge = x < full_x_start + left.edge_length;
            let right_edge = x > full_x_end - right.edge_length;
            let mut edge_flags = 0u8;
            if left_edge {
                edge_flags |= 1;
            } else if right_edge {
                // GX renders the scanline in three consecutive parts.
                // When the edge spans overlap, the left-edge loop consumes
                // the pixel before the right-edge loop can see it.
                edge_flags |= 2;
            }
            if y == y_min {
                edge_flags |= 4;
            } else if y == y_max - 1 {
                // A one-row polygon is tagged as its top edge: the hardware
                // tests top first and bottom only in the `else` arm.
                edge_flags |= 8;
            }
            if surface.alpha == 0 && edge_flags == 0 {
                continue;
            }
            let depth = interpolate_depth(
                left.depth,
                right.depth,
                position,
                interpolation_start,
                interpolation_end,
                left.w,
                right.w,
                false,
                frame.depth_mode,
            )
            .clamp(0, i64::from(FAR_DEPTH)) as u32;
            let pixel = y as usize * WIDTH + x as usize;
            let top_passes = depth_passes(
                frame.depth_mode,
                surface.depth_equal,
                buffer.depth[pixel],
                depth,
            );
            if is_shadow_mask {
                if !top_passes {
                    buffer.stencil[pixel] |= 1;
                }
                if buffer.edge_flags[pixel] != 0
                    && !depth_passes(
                        frame.depth_mode,
                        surface.depth_equal,
                        buffer.under_depth[pixel],
                        depth,
                    )
                {
                    buffer.stencil[pixel] |= 2;
                }
                continue;
            }
            let under = if top_passes {
                false
            } else if buffer.edge_flags[pixel] != 0
                && depth_passes(
                    frame.depth_mode,
                    surface.depth_equal,
                    buffer.under_depth[pixel],
                    depth,
                )
            {
                true
            } else {
                continue;
            };
            let shadow = surface.mode == 3 && surface.polygon_id != 0;
            let target_opaque_id = if under {
                buffer.under_opaque_polygon[pixel]
            } else {
                buffer.opaque_polygon[pixel]
            };
            if shadow
                && (buffer.stencil[pixel] == 0 || target_opaque_id == (surface.polygon_id & 0x3f))
            {
                continue;
            }
            let uv = [
                attr(left.uv[0], right.uv[0], position),
                attr(left.uv[1], right.uv[1], position),
            ];
            let color = [
                (attr(left.color[0], right.color[0], position) >> 3).clamp(0, 63),
                (attr(left.color[1], right.color[1], position) >> 3).clamp(0, 63),
                (attr(left.color[2], right.color[2], position) >> 3).clamp(0, 63),
            ];
            let native = render_pixel(surface, frame, uv, color);
            let alpha5 = native[3];
            if alpha5 <= frame.alpha_test.min(31) {
                continue;
            }
            let opaque = alpha5 == 31;
            let coverage = if !opaque || !frame.antialiasing || edge_flags == 0 {
                0xff
            } else if left_edge {
                if left.x_major {
                    let value = (left_coverage >> 5).clamp(0, 31) as u8;
                    left_coverage += left.coverage_increment;
                    value
                } else {
                    left.coverage_start as u8
                }
            } else if right_edge {
                if right.x_major {
                    let value = (31 - (right_coverage >> 5)).clamp(0, 31) as u8;
                    right_coverage += right.coverage_increment;
                    value
                } else {
                    right.coverage_start as u8
                }
            } else {
                31
            };
            if opaque && frame.antialiasing && !under {
                if edge_flags != 0 {
                    buffer.under_color[pixel] = buffer.color[pixel];
                    buffer.under_depth[pixel] = buffer.depth[pixel];
                    buffer.under_opaque_polygon[pixel] = buffer.opaque_polygon[pixel];
                    buffer.under_translucent_polygon[pixel] = buffer.translucent_polygon[pixel];
                    buffer.under_fogged[pixel] = buffer.fogged[pixel];
                    buffer.under_coverage[pixel] = buffer.coverage[pixel];
                    buffer.under_edge_flags[pixel] = buffer.edge_flags[pixel];
                    buffer.coverage[pixel] = coverage;
                } else {
                    buffer.coverage[pixel] = 0xFF;
                }
            }
            if opaque {
                if under {
                    buffer.under_coverage[pixel] = coverage;
                    blend(&mut buffer.under_color[pixel], native, frame.alpha_blending);
                    buffer.under_depth[pixel] = depth;
                    buffer.under_opaque_polygon[pixel] = surface.polygon_id & 0x3f;
                    buffer.under_translucent_polygon[pixel] = 0xff;
                    buffer.under_fogged[pixel] = surface.fog;
                    buffer.under_edge_flags[pixel] = edge_flags;
                } else {
                    blend(&mut buffer.color[pixel], native, frame.alpha_blending);
                    buffer.depth[pixel] = depth;
                    buffer.opaque_polygon[pixel] = surface.polygon_id & 0x3f;
                    buffer.translucent_polygon[pixel] = 0xff;
                    buffer.fogged[pixel] = surface.fog;
                    buffer.edge_flags[pixel] = edge_flags;
                }
            } else {
                let depth_write = surface.depth_write.then_some(depth);
                let drawn = plot_translucent(
                    buffer,
                    pixel,
                    under,
                    native,
                    surface,
                    depth_write,
                    frame.alpha_blending,
                );
                if drawn && !under && buffer.edge_flags[pixel] != 0 {
                    plot_translucent(
                        buffer,
                        pixel,
                        true,
                        native,
                        surface,
                        depth_write,
                        frame.alpha_blending,
                    );
                }
            }
        }
    }
}

pub(crate) fn texture_coordinate(value: i64, size: usize, repeat: bool, flip: bool) -> usize {
    let value = value >> 4;
    let size = size as i64;
    if !repeat {
        return value.clamp(0, size - 1) as usize;
    }
    let coordinate = value.rem_euclid(size);
    if flip && value.div_euclid(size) & 1 != 0 {
        (size - 1 - coordinate) as usize
    } else {
        coordinate as usize
    }
}

fn sample(surface: &Surface<'_>, uv: [i64; 2]) -> [u8; 4] {
    let Some(texture) = surface.texture else {
        return [255; 4];
    };
    if texture.width == 0 || texture.height == 0 {
        return [0; 4];
    }
    let x = texture_coordinate(
        uv[0],
        texture.width,
        surface.flags & (1 << 16) != 0,
        surface.flags & (1 << 18) != 0,
    );
    let y = texture_coordinate(
        uv[1],
        texture.height,
        surface.flags & (1 << 17) != 0,
        surface.flags & (1 << 19) != 0,
    );
    if let Some(raw) = &texture.raw {
        let pixel = y * texture.width + x;
        let byte = raw.texels.get(match texture.format {
            TexFmt::Pltt4 => pixel / 4,
            TexFmt::Pltt16 => pixel / 2,
            TexFmt::A3i5 | TexFmt::Pltt256 | TexFmt::A5i3 => pixel,
        });
        let Some(&byte) = byte else {
            return [0; 4];
        };
        let (index, mut alpha) = match texture.format {
            TexFmt::Pltt4 => ((byte >> ((pixel & 3) * 2)) & 3, 31),
            TexFmt::Pltt16 => ((byte >> ((pixel & 1) * 4)) & 15, 31),
            TexFmt::Pltt256 => (byte, 31),
            TexFmt::A3i5 => {
                let alpha3 = byte >> 5;
                (byte & 31, (alpha3 << 2) | (alpha3 >> 1))
            }
            TexFmt::A5i3 => (byte & 7, byte >> 3),
        };
        // TEXIMAGE_PARAM bit 29 only applies to the indexed opaque formats;
        // A3I5/A5I3 always take alpha from the texel, including palette zero.
        if index == 0
            && texture.color0_transparent
            && matches!(
                texture.format,
                TexFmt::Pltt4 | TexFmt::Pltt16 | TexFmt::Pltt256
            )
        {
            alpha = 0;
        }
        let Some(&palette) = raw.palette.get(usize::from(index)) else {
            return [0; 4];
        };
        let color = native_color(palette);
        return [color[0], color[1], color[2], alpha];
    }
    let rgba = texture.pixels[y * texture.width + x];
    let color5 = |channel: u8| ((u16::from(channel) * 31 + 127) / 255) as u8;
    let color6 = |channel: u8| {
        let value = color5(channel);
        if value == 0 { 0 } else { (value << 1) | 1 }
    };
    let alpha = match texture.format {
        TexFmt::A3i5 => {
            let value = ((u16::from(rgba[3]) * 7 + 127) / 255) as u8;
            (value << 2) | (value >> 1)
        }
        TexFmt::A5i3 => ((u16::from(rgba[3]) * 31 + 127) / 255) as u8,
        TexFmt::Pltt4 | TexFmt::Pltt16 | TexFmt::Pltt256 => {
            if rgba[3] == 0 {
                0
            } else {
                31
            }
        }
    };
    [color6(rgba[0]), color6(rgba[1]), color6(rgba[2]), alpha]
}

fn expand_5_to_6(value: i64) -> u8 {
    let value = value.clamp(0, 31) as u8;
    if value == 0 { 0 } else { (value << 1) | 1 }
}

fn native_color(value: u16) -> [u8; 3] {
    [
        expand_5_to_6(i64::from(value & 31)),
        expand_5_to_6(i64::from((value >> 5) & 31)),
        expand_5_to_6(i64::from((value >> 10) & 31)),
    ]
}

fn render_pixel(
    surface: &Surface<'_>,
    frame: &Gx3dFrame,
    uv: [i64; 2],
    color: [i64; 3],
) -> [u8; 4] {
    let mut vertex = color.map(|channel| channel.clamp(0, 63) as u8);
    let toon = native_color(frame.toon_table[usize::from(vertex[0] >> 1)]);
    if surface.mode == 2 {
        if frame.highlight_shading {
            vertex[1] = vertex[0];
            vertex[2] = vertex[0];
        } else {
            vertex = toon;
        }
    }
    let mut out = if surface.texture.is_some() {
        let texel = sample(surface, uv);
        if surface.mode & 1 != 0 {
            let rgb = if texel[3] == 0 {
                vertex
            } else if texel[3] == 31 {
                [texel[0], texel[1], texel[2]]
            } else {
                std::array::from_fn(|channel| {
                    ((u16::from(texel[channel]) * u16::from(texel[3])
                        + u16::from(vertex[channel]) * u16::from(31 - texel[3]))
                        >> 5) as u8
                })
            };
            [rgb[0], rgb[1], rgb[2], surface.alpha.min(31)]
        } else {
            let rgb: [u8; 3] = std::array::from_fn(|channel| {
                ((u16::from(texel[channel] + 1) * u16::from(vertex[channel] + 1) - 1) >> 6) as u8
            });
            let alpha =
                ((u16::from(texel[3] + 1) * u16::from(surface.alpha.min(31) + 1) - 1) >> 5) as u8;
            [rgb[0], rgb[1], rgb[2], alpha]
        }
    } else {
        [vertex[0], vertex[1], vertex[2], surface.alpha.min(31)]
    };
    if surface.mode == 2 && frame.highlight_shading {
        for channel in 0..3 {
            out[channel] = out[channel].saturating_add(toon[channel]).min(63);
        }
    }
    if surface.alpha == 0 {
        out[3] = 31;
    }
    out
}

fn blend(destination: &mut [u8; 4], source: [u8; 4], enabled: bool) {
    let to_6 = |value: u8| ((u16::from(value) * 63 + 127) / 255) as u8;
    let from_6 = |value: u8| (value << 2) | (value >> 4);
    let to_5 = |value: u8| ((u16::from(value) * 31 + 127) / 255) as u8;
    let from_5 = |value: u8| (u16::from(value) * 255 / 31) as u8;
    let source_alpha = source[3];
    if destination[3] == 0 {
        *destination = [
            from_6(source[0]),
            from_6(source[1]),
            from_6(source[2]),
            from_5(source_alpha),
        ];
        return;
    }
    if enabled {
        let weight = u16::from(source_alpha) + 1;
        for channel in 0..3 {
            let mixed = (u16::from(source[channel]) * weight
                + u16::from(to_6(destination[channel])) * (32 - weight))
                >> 5;
            destination[channel] = from_6(mixed as u8);
        }
    } else {
        for channel in 0..3 {
            destination[channel] = from_6(source[channel]);
        }
    }
    destination[3] = from_5(to_5(destination[3]).max(source_alpha));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vertex(x: i32, y: i32, depth: u32) -> Projected {
        Projected {
            x,
            y,
            depth,
            w: 4096,
            uv: [0; 2],
            color: [0x1FF; 3],
            winding: 0,
        }
    }

    #[test]
    fn adjacent_integer_quads_share_a_boundary_without_a_hole() {
        let frame = Gx3dFrame::default();
        let surface = Surface {
            texture: None,
            flags: 0,
            alpha: 31,
            polygon_id: 1,
            fog: false,
            depth_equal: false,
            depth_write: false,
            mode: 0,
        };
        let mut buffer = Gx3dBuffer::new();
        draw_quad(
            [
                vertex(10, 10, 1),
                vertex(20, 10, 1),
                vertex(20, 20, 1),
                vertex(10, 20, 1),
            ],
            &surface,
            true,
            &frame,
            &mut buffer,
        );
        draw_quad(
            [
                vertex(20, 10, 1),
                vertex(30, 10, 1),
                vertex(30, 20, 1),
                vertex(20, 20, 1),
            ],
            &surface,
            true,
            &frame,
            &mut buffer,
        );
        for y in 10..20 {
            for x in 10..30 {
                assert_ne!(buffer.colors()[y * WIDTH + x][3], 0, "({x}, {y})");
            }
        }
    }

    #[test]
    fn adjacent_x_major_edges_share_every_scanline() {
        let surface = Surface {
            texture: None,
            flags: 0,
            alpha: 31,
            polygon_id: 1,
            fog: false,
            depth_equal: false,
            depth_write: false,
            mode: 0,
        };
        let mut buffer = Gx3dBuffer::new();
        let frame = Gx3dFrame::default();
        draw_quad(
            [
                vertex(20, 20, 10),
                vertex(40, 20, 10),
                vertex(80, 100, 10),
                vertex(20, 100, 10),
            ],
            &surface,
            true,
            &frame,
            &mut buffer,
        );
        draw_quad(
            [
                vertex(40, 20, 10),
                vertex(100, 20, 10),
                vertex(100, 100, 10),
                vertex(80, 100, 10),
            ],
            &surface,
            true,
            &frame,
            &mut buffer,
        );
        for y in 20..100 {
            for x in 20..100 {
                assert_ne!(buffer.colors()[y * WIDTH + x][3], 0, "gap at ({x}, {y})");
            }
        }
    }

    #[test]
    fn depth_and_polygon_metadata_follow_the_nearest_draw() {
        let frame = Gx3dFrame::default();
        let mut buffer = Gx3dBuffer::new();
        for (depth, polygon_id, color) in [(100, 2, 0x001F), (50, 3, 0x03E0)] {
            let texture = Texture {
                width: 1,
                height: 1,
                pixels: vec![bgr555(color)],
                format: TexFmt::Pltt256,
                color0_transparent: false,
                raw: None,
            };
            let surface = Surface {
                texture: Some(&texture),
                flags: 0,
                alpha: 31,
                polygon_id,
                fog: false,
                depth_equal: false,
                depth_write: false,
                mode: 0,
            };
            draw_triangle(
                [
                    vertex(0, 0, depth),
                    vertex(8, 0, depth),
                    vertex(0, 8, depth),
                ],
                &surface,
                true,
                &frame,
                &mut buffer,
            );
        }
        assert_eq!(buffer.depths()[2 * WIDTH + 2], 50);
        assert_eq!(buffer.polygon_ids()[2 * WIDTH + 2], 3);
        assert_eq!(buffer.colors()[2 * WIDTH + 2][..3], [0, 255, 0]);
    }

    #[test]
    fn polygon_depth_equal_uses_the_z_buffer_tolerance() {
        let frame = Gx3dFrame::default();
        let mut buffer = Gx3dBuffer::new();
        let first = Surface {
            texture: None,
            flags: 0,
            alpha: 31,
            polygon_id: 4,
            fog: false,
            depth_equal: false,
            depth_write: false,
            mode: 0,
        };
        let equal = Surface {
            polygon_id: 5,
            depth_equal: true,
            depth_write: true,
            ..first
        };
        let triangle = |depth| {
            [
                vertex(0, 0, depth),
                vertex(8, 0, depth),
                vertex(0, 8, depth),
            ]
        };
        draw_triangle(triangle(1_000), &first, true, &frame, &mut buffer);
        draw_triangle(triangle(1_300), &equal, false, &frame, &mut buffer);
        assert_eq!(buffer.depths()[WIDTH + 1], 1_300);
        assert_eq!(buffer.polygon_ids()[WIDTH + 1], 5);
    }

    #[test]
    fn w_buffer_uses_reconstructed_w_and_its_own_equal_tolerance() {
        let frame = Gx3dFrame {
            depth_mode: DepthMode::W,
            ..Gx3dFrame::default()
        };
        let first = Surface {
            texture: None,
            flags: 0,
            alpha: 31,
            polygon_id: 4,
            fog: false,
            depth_equal: false,
            depth_write: false,
            mode: 0,
        };
        let equal = Surface {
            polygon_id: 5,
            depth_equal: true,
            ..first
        };
        let with_w = |w| {
            let mut vertices = [vertex(0, 0, 0), vertex(8, 0, 0), vertex(0, 8, 0)];
            for vertex in &mut vertices {
                vertex.w = w;
            }
            vertices
        };
        let mut buffer = Gx3dBuffer::new();
        draw_triangle(with_w(1_000), &first, true, &frame, &mut buffer);
        draw_triangle(with_w(1_255), &equal, false, &frame, &mut buffer);
        assert_eq!(buffer.depths()[WIDTH + 1], 1_255);
        draw_triangle(with_w(1_511), &equal, false, &frame, &mut buffer);
        assert_eq!(buffer.depths()[WIDTH + 1], 1_255, "difference 256 fails");
    }

    #[test]
    fn alpha_test_discards_before_color_and_metadata_writes() {
        let frame = Gx3dFrame {
            alpha_test: 16,
            ..Gx3dFrame::default()
        };
        let surface = Surface {
            texture: None,
            flags: 0,
            alpha: 8,
            polygon_id: 7,
            fog: false,
            depth_equal: false,
            depth_write: false,
            mode: 0,
        };
        let mut buffer = Gx3dBuffer::new();
        draw_triangle(
            [vertex(0, 0, 10), vertex(8, 0, 10), vertex(0, 8, 10)],
            &surface,
            true,
            &frame,
            &mut buffer,
        );
        assert_eq!(buffer.colors()[WIDTH + 1], [0; 4]);
        assert_eq!(buffer.depths()[WIDTH + 1], FAR_DEPTH);
        assert_eq!(buffer.polygon_ids()[WIDTH + 1], 0xff);
    }

    #[test]
    fn equal_translucent_polygon_ids_do_not_blend_twice() {
        let frame = Gx3dFrame::default();
        let red = Texture {
            width: 1,
            height: 1,
            pixels: vec![[255, 0, 0, 255]],
            format: TexFmt::Pltt256,
            color0_transparent: false,
            raw: None,
        };
        let green = Texture {
            width: 1,
            height: 1,
            pixels: vec![[0, 255, 0, 255]],
            format: TexFmt::Pltt256,
            color0_transparent: false,
            raw: None,
        };
        let base = Surface {
            texture: Some(&red),
            flags: 0,
            alpha: 15,
            polygon_id: 9,
            fog: false,
            depth_equal: false,
            depth_write: false,
            mode: 0,
        };
        let triangle = [vertex(0, 0, 10), vertex(8, 0, 10), vertex(0, 8, 10)];
        let mut buffer = Gx3dBuffer::new();
        draw_triangle(triangle, &base, false, &frame, &mut buffer);
        let first = buffer.colors()[WIDTH + 1];
        draw_triangle(
            triangle,
            &Surface {
                texture: Some(&green),
                ..base
            },
            false,
            &frame,
            &mut buffer,
        );
        assert_eq!(buffer.colors()[WIDTH + 1], first);
        assert_eq!(buffer.polygon_ids()[WIDTH + 1], 0xff);
        assert_eq!(buffer.translucent_polygon_ids()[WIDTH + 1], 9);
    }

    #[test]
    fn shadow_mask_stencils_depth_failure_before_shadow_blends() {
        let frame = Gx3dFrame::default();
        let triangle = [vertex(0, 0, 100), vertex(8, 0, 100), vertex(0, 8, 100)];
        let receiver = Surface {
            texture: None,
            flags: 0,
            alpha: 31,
            polygon_id: 2,
            fog: false,
            depth_equal: false,
            depth_write: false,
            mode: 0,
        };
        let mask = Surface {
            polygon_id: 0,
            mode: 3,
            ..receiver
        };
        let shadow = Surface {
            alpha: 15,
            polygon_id: 3,
            mode: 3,
            ..receiver
        };
        let mut buffer = Gx3dBuffer::new();
        draw_triangle(triangle, &receiver, true, &frame, &mut buffer);
        draw_triangle(
            [vertex(0, 0, 200), vertex(8, 0, 200), vertex(0, 8, 200)],
            &mask,
            false,
            &frame,
            &mut buffer,
        );
        draw_triangle(
            [vertex(0, 0, 50), vertex(8, 0, 50), vertex(0, 8, 50)],
            &shadow,
            false,
            &frame,
            &mut buffer,
        );
        assert_ne!(buffer.stencil[WIDTH + 1], 0);
        assert_eq!(buffer.polygon_ids()[WIDTH + 1], 2);
        assert_eq!(buffer.translucent_polygon_ids()[WIDTH + 1], 3);
    }

    #[test]
    fn fog_density_uses_offset_shift_and_interpolated_table() {
        let mut frame = Gx3dFrame {
            fog_offset: 100,
            fog_shift: 0,
            ..Gx3dFrame::default()
        };
        frame.fog_table[0] = 10;
        frame.fog_table[1] = 30;
        assert_eq!(fog_density(&frame, 99), 10);
        assert_eq!(fog_density(&frame, 100), 10);
        assert!(fog_density(&frame, 100 + (3 << 18)) > 10);
    }

    #[test]
    fn antialias_resolve_uses_edge_coverage_and_lower_color() {
        let frame = Gx3dFrame {
            antialiasing: true,
            ..Gx3dFrame::default()
        };
        let surface = Surface {
            texture: None,
            flags: 0,
            alpha: 31,
            polygon_id: 1,
            fog: false,
            depth_equal: false,
            depth_write: false,
            mode: 0,
        };
        let mut buffer = Gx3dBuffer::new();
        draw_triangle(
            [vertex(10, 10, 10), vertex(40, 20, 10), vertex(12, 50, 10)],
            &surface,
            true,
            &frame,
            &mut buffer,
        );
        assert!(buffer.coverages().iter().any(|&coverage| coverage < 31));
        buffer.resolve(&frame);
        assert!(
            buffer
                .colors()
                .iter()
                .any(|pixel| pixel[3] > 0 && pixel[3] < 255)
        );
    }

    #[test]
    fn failed_top_depth_test_can_render_into_the_antialias_lower_layer() {
        let frame = Gx3dFrame::default();
        let pixel = WIDTH + 1;
        let mut buffer = Gx3dBuffer::new();
        buffer.color[pixel] = [255, 0, 0, 255];
        buffer.depth[pixel] = 10;
        buffer.edge_flags[pixel] = 1;
        buffer.coverage[pixel] = 15;
        buffer.under_depth[pixel] = 100;
        let green = Texture {
            width: 1,
            height: 1,
            pixels: vec![[0, 255, 0, 255]],
            format: TexFmt::Pltt256,
            color0_transparent: false,
            raw: None,
        };
        let surface = Surface {
            texture: Some(&green),
            flags: 0,
            alpha: 31,
            polygon_id: 2,
            fog: false,
            depth_equal: false,
            depth_write: false,
            mode: 0,
        };
        draw_triangle(
            [vertex(0, 0, 50), vertex(8, 0, 50), vertex(0, 8, 50)],
            &surface,
            true,
            &frame,
            &mut buffer,
        );
        assert_eq!(buffer.color[pixel], [255, 0, 0, 255]);
        assert_eq!(buffer.under_color[pixel][..3], [0, 255, 0]);
        assert_eq!(buffer.under_depth[pixel], 50);
    }

    #[test]
    fn edge_marking_precedes_fog_and_native_planes_keep_clear_metadata() {
        let mut frame = Gx3dFrame {
            edge_marking: true,
            fog: true,
            fog_color: 0x7c00,
            fog_alpha: 31,
            fog_table: [127; 32],
            ..Gx3dFrame::default()
        };
        frame.edge_colors[0] = 0x001f;
        let mut buffer = Gx3dBuffer::new();
        buffer.clear_with_frame(&frame);
        assert_eq!(buffer.native_attributes()[0], 0x3f00_0000);
        let pixel = WIDTH + 1;
        buffer.color[pixel] = [255; 4];
        buffer.depth[pixel] = 1;
        buffer.opaque_polygon[pixel] = 1;
        buffer.edge_flags[pixel] = 1;
        buffer.fogged[pixel] = true;
        buffer.resolve(&frame);
        assert_eq!(buffer.color[pixel], [0, 0, 255, 255]);
        assert_eq!(buffer.native_colors()[pixel], 0x1f3f_0000);
    }

    #[test]
    fn alpha_zero_polygon_is_wireframe() {
        let frame = Gx3dFrame::default();
        let surface = Surface {
            texture: None,
            flags: 0,
            alpha: 0,
            polygon_id: 1,
            fog: false,
            depth_equal: false,
            depth_write: false,
            mode: 0,
        };
        let mut buffer = Gx3dBuffer::new();
        draw_quad(
            [
                vertex(10, 10, 10),
                vertex(30, 10, 10),
                vertex(30, 30, 10),
                vertex(10, 30, 10),
            ],
            &surface,
            true,
            &frame,
            &mut buffer,
        );
        assert_ne!(buffer.colors()[10 * WIDTH + 10][3], 0);
        assert_eq!(buffer.colors()[20 * WIDTH + 20][3], 0);
    }

    #[test]
    fn raw_a3i5_texel_keeps_native_palette_and_alpha() {
        let texture = Texture {
            width: 1,
            height: 1,
            pixels: vec![[0; 4]],
            format: TexFmt::A3i5,
            color0_transparent: false,
            raw: Some(apricorn_core::field::model::RawTexture {
                texels: vec![(5 << 5) | 1],
                palette: vec![0, 0x001F],
            }),
        };
        let surface = Surface {
            texture: Some(&texture),
            flags: 0,
            alpha: 31,
            polygon_id: 0,
            fog: false,
            depth_equal: false,
            depth_write: false,
            mode: 0,
        };
        assert_eq!(sample(&surface, [0, 0]), [63, 0, 0, 22]);
    }

    #[test]
    fn palette_zero_transparency_does_not_override_explicit_texel_alpha() {
        for (format, byte, alpha) in [
            (TexFmt::Pltt4, 0, 0),
            (TexFmt::Pltt16, 0, 0),
            (TexFmt::Pltt256, 0, 0),
            (TexFmt::A3i5, 5 << 5, 22),
            (TexFmt::A5i3, 17 << 3, 17),
        ] {
            let texture = Texture {
                width: 1,
                height: 1,
                pixels: vec![[0; 4]],
                format,
                color0_transparent: true,
                raw: Some(apricorn_core::field::model::RawTexture {
                    texels: vec![byte],
                    palette: vec![0x001f],
                }),
            };
            let surface = Surface {
                texture: Some(&texture),
                flags: 0,
                alpha: 31,
                polygon_id: 0,
                fog: false,
                depth_equal: false,
                depth_write: false,
                mode: 0,
            };
            assert_eq!(sample(&surface, [0, 0]), [63, 0, 0, alpha], "{format:?}");
        }
    }

    #[test]
    fn one_scanline_polygon_has_top_but_not_bottom_metadata() {
        let frame = Gx3dFrame {
            antialiasing: true,
            ..Gx3dFrame::default()
        };
        let surface = Surface {
            texture: None,
            flags: 0,
            alpha: 31,
            polygon_id: 1,
            fog: false,
            depth_equal: false,
            depth_write: false,
            mode: 0,
        };
        let mut buffer = Gx3dBuffer::new();
        draw_quad(
            [
                vertex(10, 10, 1),
                vertex(30, 10, 1),
                vertex(30, 11, 1),
                vertex(10, 11, 1),
            ],
            &surface,
            true,
            &frame,
            &mut buffer,
        );
        for x in 10..30 {
            assert_ne!(buffer.colors()[10 * WIDTH + x][3], 0);
            assert_eq!(buffer.edge_flags[10 * WIDTH + x] & 12, 4);
        }
    }
}

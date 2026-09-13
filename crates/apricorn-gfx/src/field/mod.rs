//! The field (3D) layer — the picture engine A's BG0 shows while a map
//! is up, rasterized on the CPU from a [`SceneView`].
//!
//! # What the original does
//!
//! The field binds BG0 to the 3D core (`GX_BG0_AS_3D`,
//! `src/field/fieldmap.c:514`), turns the plane on once the map is
//! loaded (`:749`), and draws every frame in `ov01_021E6220`
//! (`:580-613`): reset, push the camera (`Camera_PushLookAtToNNSGlb`),
//! draw the loaded map cells (`MapLoadManager_RenderLoadedMaps`), the
//! props (`ov01_021F3C9C`), then — with the projection's `_32` biased
//! by `8 · cos(−angle.x)` — the field effects and the billboard lists,
//! restore the projection, draw the 3D object tasks, and swap with
//! `GX_SORTMODE_AUTO` and the camera's Z-buffer mode. The hardware
//! then draws opaque polygons first and translucent ones after, depth
//! tested against them without writing depth, and clears to alpha 0
//! (`G3X_SetClearColor(RGB_BLACK, 0, …)`, `src/gf_3d_vramman.c:60`),
//! so uncovered pixels are transparent to the 2D layers below.
//!
//! # This port
//!
//! [`render_view_gx`] draws a [`SceneView`] into a native 256×192
//! [`Gx3dBuffer`]: opaque meshes, billboards through the biased
//! projection, then translucent meshes without depth writes. Camera
//! transformation, viewport division, polygon scan conversion, depth,
//! and perspective attributes are integer/fixed-point. Triangles and
//! quads retain their GX primitive identity, so quads acquire no host
//! diagonal. [`render_view`] remains as a compatibility wrapper which
//! converts the finished 24-bit depth plane to `f64`; the production
//! compositor calls [`render_view_gx`] directly.
//!
//! `raster.rs` composites the result as BG0: `bgs[0].priority` and
//! `enabled` apply, as do the other layers, OBJ, the hardware window
//! (`hidden_rect`), blending (a 3D pixel blends with the second target
//! by its own alpha), backdrop and master brightness.
//!
//! # Seams for the map-data agent's richer `FieldScene`
//!
//! * **Cells** — the loaded 32×32-tile map cells are world-space
//!   [`Mesh`]es: parse each land model with its matrix-cell origin
//!   (`x · 512, 0, z · 512` fx32 per cell, as `FieldScene::bedroom`
//!   centres its one cell) and pass all of them as `SceneView::meshes`.
//! * **Props** — `MapPropArcData { model, translation, rotation,
//!   scale }` become world meshes too: pret builds the model matrix as
//!   `RotX(rotation.x) · RotY(rotation.y) · RotZ(rotation.z)` with each
//!   fx32 component's low 16 bits as a 16-bit angle
//!   (`sub_02020D2C`, `asm/unk_02020B8C.s:218`), scaled by `scale` and
//!   translated (`GF3dRender_DrawModel`, `src/gf_3d_render.c:34`). Bake
//!   that into the vertices ([`camera::sin_idx`]/[`camera::cos_idx`]
//!   give the same table values) and pass the result as meshes.
//! * **Billboards** — every map object is a [`BillboardView`]: its
//!   decoded NSBTX from `a/0/8/1` (`FieldScene::bedroom` decodes one
//!   with `model::decode_texture`), the frame's texel rect, the
//!   object's position vector, and the size class' quad size.
//! * **Camera** — [`CameraPreset`] is the gfx-side shape of the ROM's
//!   `ov01_02206478` row; `CameraPreset::from_table_entry` reads the
//!   0x24 bytes, and `Camera::from_preset(preset, target)` takes the
//!   player's position vector as the target.
//!
//! [`render`] draws a [`FieldFrame`] — the per-tick view the field
//! system publishes (`docs/field-system.md`): the scene's meshes, the
//! frame's camera preset resolved at its target, and one billboard per
//! [`ObjectView`].

pub mod billboard;
pub mod camera;

pub use billboard::BillboardView;
pub use camera::{Camera, CameraPreset, Projection};

use crate::gx3d::{
    Gx3dBuffer, Gx3dFrame, Projected as GxProjected, Surface as GxSurface,
    draw_clipped_polygon as gx_draw_clipped_polygon,
};
use apricorn_core::field::{
    lighting::ModelLighting,
    model::{Mesh, Vertex},
    ov01,
};
use apricorn_core::formats::TexFmt;
use apricorn_core::frame::FieldFrame;
use camera::FX32_ONE;

/// The 3D viewport width in pixels.
pub const WIDTH: usize = 256;
/// The 3D viewport height in pixels.
pub const HEIGHT: usize = 192;

/// One frame's worth of field to draw.
#[derive(Debug, Clone, PartialEq)]
pub struct SceneView<'a> {
    /// World-space geometry — map cells and props — in draw order.
    pub meshes: &'a [Mesh],
    /// Prefix of `meshes` originating in independently decoded land
    /// cells. Their shared boundaries must remain covered after GX
    /// quantization; later meshes are freestanding props/silhouettes.
    pub land_mesh_count: usize,
    /// Multiple independently decoded outdoor cells can expose a
    /// one-pixel clear crack at a shared quantized boundary.
    pub seal_land_cracks: bool,
    /// Map-object billboards, drawn after the meshes with the field's
    /// depth bias.
    pub billboards: Vec<BillboardView<'a>>,
    /// The resolved camera.
    pub camera: Camera,
    /// Active global GX light/material registers for this field tick.
    pub lighting: Option<ModelLighting>,
}

/// The player's position vector for a tile: the tile centre, in fx32
/// (`PlayerAvatar_GetPositionVector`; one tile is 16 world units).
#[must_use]
pub fn tile_position(tile: [i32; 2]) -> [i32; 3] {
    [
        (tile[0] * 16 + 8) * FX32_ONE,
        0,
        (tile[1] * 16 + 8) * FX32_ONE,
    ]
}

/// The core crate's camera preset (the ROM row as `field::ov01` reads
/// it) in this crate's shape: the same fields, `perspectiveType`
/// mapped onto [`Projection`] as `Camera_ApplyPerspectiveType` reads
/// it (0 perspective, anything else orthographic).
#[must_use]
pub fn preset_from_core(preset: &ov01::CameraPreset) -> CameraPreset {
    CameraPreset {
        distance: preset.distance,
        angle: preset.angle,
        projection: if preset.perspective_type == 0 {
            Projection::Perspective
        } else {
            Projection::Orthographic
        },
        fovy: preset.fovy_angle,
        near: preset.near,
        far: preset.far,
        look_at_offset: preset.look_at_offset,
    }
}

/// A [`FieldFrame`] as a view: the scene's meshes, the frame's preset
/// resolved at its camera target, and one billboard per object.
#[must_use]
pub fn scene_view(field: &FieldFrame) -> SceneView<'_> {
    let land_mesh_count = field.scene.cells.iter().map(|cell| cell.meshes.len()).sum();
    SceneView {
        meshes: &field.scene.meshes,
        land_mesh_count,
        seal_land_cracks: true,
        billboards: field
            .objects
            .iter()
            .map(|object| BillboardView {
                texture: &object.texture,
                rect: object.rect,
                world_pos: object.world_pos,
                size_px: object.size_px,
                mirrored: object.mirrored,
            })
            .collect(),
        camera: Camera::from_preset(&preset_from_core(&field.camera), field.camera_target),
        lighting: field.lighting,
    }
}

/// Renders a [`FieldFrame`] through [`scene_view`] into `out` (RGBA8,
/// alpha = coverage) and `depth` (NDC depth, `+∞` where clear).
///
/// # Panics
/// Panics unless both buffers hold exactly 256×192 entries.
pub fn render(field: &FieldFrame, out: &mut [[u8; 4]], depth: &mut [f64]) {
    render_view(&scene_view(field), out, depth);
}

/// Renders a view into `out` (RGBA8; alpha 0 where no polygon covered
/// the pixel, the polygon's alpha otherwise) and `depth` (NDC depth,
/// `+∞` where clear).
///
/// # Panics
/// Panics unless both buffers hold exactly 256×192 entries.
pub fn render_view(view: &SceneView<'_>, out: &mut [[u8; 4]], depth: &mut [f64]) {
    assert_eq!(out.len(), WIDTH * HEIGHT, "a 256×192 colour buffer");
    assert_eq!(depth.len(), WIDTH * HEIGHT, "a 256×192 depth buffer");
    let mut buffer = Gx3dBuffer::new();
    let frame = field_registers(view);
    render_view_gx(view, &frame, &mut buffer);
    out.copy_from_slice(buffer.colors());
    for (destination, &source) in depth.iter_mut().zip(buffer.depths()) {
        *destination = if source == 0xFF_FFFF {
            f64::INFINITY
        } else {
            f64::from(source) / f64::from(0xFF_FFFF)
        };
    }
}

/// GX display state used by HeartGold's live field renderer.
///
/// The raw retail oracle is authoritative here: active field frames
/// carry edge coverage and resolve it even though an earlier setup path
/// calls `G3X_AntiAlias(FALSE)` before the field becomes live.
#[must_use]
pub fn field_registers(view: &SceneView<'_>) -> Gx3dFrame {
    Gx3dFrame {
        antialiasing: true,
        edge_marking: true,
        edge_colors: [0, 0x1084, 0x1084, 0x1084, 0x1084, 0x1084, 0x1084, 0x1084],
        lighting: view.lighting,
        ..Gx3dFrame::default()
    }
}

/// Renders a field view into the native DS colour, depth and polygon
/// planes using the supplied 3D display-control state.
pub fn render_view_gx(view: &SceneView<'_>, frame: &Gx3dFrame, buffer: &mut Gx3dBuffer) {
    buffer.clear_with_frame(frame);
    let mut polygons = Vec::new();
    for (index, mesh) in view.meshes.iter().enumerate() {
        collect_mesh(
            mesh,
            &view.camera,
            frame,
            view.seal_land_cracks && index < view.land_mesh_count,
            &mut polygons,
        );
    }
    // Billboard lists are submitted after the map, but GX auto-sort still
    // places them by the same opaque/translucent and Y bounds key.
    for billboard in &view.billboards {
        let Some(corners) = billboard.corners(&view.camera) else {
            continue;
        };
        let vertices = corners.into_iter().map(Into::into).collect::<Vec<_>>();
        let translucent = matches!(billboard.texture.format, TexFmt::A3i5 | TexFmt::A5i3);
        polygons.push(DrawCommand::new(
            vertices,
            GxSurface {
                texture: Some(billboard.texture),
                flags: 0,
                alpha: 31,
                polygon_id: 0,
                fog: true,
                depth_equal: false,
                depth_write: false,
                mode: 0,
            },
            translucent,
            false,
        ));
    }
    // Auto mode orders by opaque/translucent and Y bounds. Manual mode
    // retains submission order inside the two hardware render passes.
    if frame.auto_sort {
        polygons.sort_by_key(|polygon| polygon.sort_key);
    } else {
        polygons.sort_by_key(|polygon| polygon.translucent);
    }
    for polygon in polygons {
        gx_draw_clipped_polygon(
            &polygon.vertices,
            &polygon.surface,
            !polygon.translucent,
            polygon.fill_all_edges,
            frame,
            buffer,
        );
    }
    buffer.resolve(frame);
}

struct DrawCommand<'a> {
    vertices: Vec<GxProjected>,
    surface: GxSurface<'a>,
    translucent: bool,
    fill_all_edges: bool,
    sort_key: u32,
}

impl<'a> DrawCommand<'a> {
    fn new(
        vertices: Vec<GxProjected>,
        surface: GxSurface<'a>,
        translucent: bool,
        fill_all_edges: bool,
    ) -> Self {
        let top = vertices
            .iter()
            .map(|vertex| vertex.y)
            .min()
            .unwrap_or(0)
            .clamp(0, 192);
        let bottom = vertices
            .iter()
            .map(|vertex| vertex.y)
            .max()
            .unwrap_or(0)
            .clamp(0, 192);
        Self {
            vertices,
            surface,
            translucent,
            fill_all_edges,
            sort_key: (u32::from(translucent) << 16) | ((bottom as u32) << 8) | top as u32,
        }
    }
}

/// Projects one mesh's polygons and retains their hardware auto-sort keys.
fn collect_mesh<'a>(
    mesh: &'a Mesh,
    camera: &Camera,
    frame: &Gx3dFrame,
    fill_all_edges: bool,
    out: &mut Vec<DrawCommand<'a>>,
) {
    let base_surface = GxSurface {
        texture: mesh.texture.as_deref(),
        flags: mesh.texture_flags,
        alpha: mesh.alpha,
        polygon_id: ((mesh.polygon_attr >> 24) & 0x3f) as u8,
        fog: mesh.polygon_attr & (1 << 15) != 0,
        depth_equal: mesh.polygon_attr & (1 << 14) != 0,
        depth_write: mesh.polygon_attr & (1 << 11) != 0,
        mode: ((mesh.polygon_attr >> 4) & 3) as u8,
    };
    let translucent = mesh.alpha > 0
        && (mesh.alpha < 31
            || mesh
                .texture
                .as_ref()
                .is_some_and(|texture| matches!(texture.format, TexFmt::A3i5 | TexFmt::A5i3)));
    let model_camera = camera.with_model(&mesh.gx_transform);
    for primitive in &mesh.primitives {
        let mut vertices = primitive.vertices().to_vec();
        if let Some(lighting) = frame.lighting {
            for vertex in &mut vertices {
                if let Some(normal) = vertex.normal {
                    vertex.color = light_vertex(mesh, camera, frame, lighting, normal);
                }
            }
        }
        for vertex in &mut vertices {
            vertex.position = vertex.gx_position;
        }
        if let Some(projected) = clip_and_project(&model_camera, &vertices, mesh.polygon_attr) {
            out.push(DrawCommand::new(
                projected,
                base_surface,
                translucent,
                fill_all_edges,
            ));
        }
    }
}

fn color_channels(color: u16) -> [i32; 3] {
    [
        i32::from(color & 31),
        i32::from((color >> 5) & 31),
        i32::from((color >> 10) & 31),
    ]
}

fn sign_extend_11(value: i32) -> i32 {
    (value << 21) >> 21
}

/// Integer GX normal lighting. Products are truncated at the same
/// boundaries as the geometry engine: signed 1.9 dot products, 20-bit
/// diffuse contributions and 14 fractional accumulator bits.
fn light_vertex(
    mesh: &Mesh,
    camera: &Camera,
    frame: &Gx3dFrame,
    lighting: ModelLighting,
    normal: [i16; 3],
) -> u16 {
    let normal = camera.to_camera_normal(normal);
    let diffuse = color_channels(lighting.diffuse);
    let ambient = color_channels(lighting.ambient);
    let specular = color_channels(lighting.specular);
    let emission = color_channels(lighting.emission);
    let mut accumulator = emission.map(|channel| i64::from(channel) << 14);

    for light_index in 0..4 {
        if mesh.polygon_attr & (1 << light_index) == 0 {
            continue;
        }
        let (light, denominator) = camera.to_camera_light(lighting.light_vectors[light_index]);
        let light_color = color_channels(lighting.light_colors[light_index]);
        let mut dot =
            (light[0] * normal[0] >> 9) + (light[1] * normal[1] >> 9) + (light[2] * normal[2] >> 9);
        let mut shine = 0;
        if dot > 0 {
            let diffuse_dot = sign_extend_11(dot);
            for channel in 0..3 {
                let contribution = diffuse[channel] * light_color[channel] * diffuse_dot;
                accumulator[channel] += i64::from(contribution & 0xF_FFFF);
            }
            dot = sign_extend_11(dot + normal[2]);
            let squared = ((dot * dot) >> 10) & 0x3FF;
            let reciprocal = if denominator == 0 {
                0
            } else {
                (1 << 18) / denominator
            };
            shine = ((squared * reciprocal) >> 8) - 512;
            shine = sign_extend_11(shine).clamp(0, 0x1FF);
        }
        let use_table = mesh.specular_emission & 0x8000 != 0 || lighting.specular_shininess;
        if use_table {
            shine = i32::from(frame.shininess_table[(shine >> 2) as usize]) << 1;
        }
        for channel in 0..3 {
            accumulator[channel] += i64::from(
                (specular[channel] * shine + (ambient[channel] << 9)) * light_color[channel],
            );
        }
    }

    let channels = accumulator.map(|value| ((value >> 14).clamp(0, 31)) as u16);
    channels[0] | (channels[1] << 5) | (channels[2] << 10)
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct ClipVertex {
    position: camera::ClipPosition,
    uv: [i64; 2],
    color: [i64; 3],
}

impl ClipVertex {
    fn interpolate(self, other: Self, numerator: i64, denominator: i64) -> Self {
        let mix = |a: i64, b: i64| {
            a + ((i128::from(b - a) * i128::from(numerator)) / i128::from(denominator)) as i64
        };
        Self {
            position: camera::ClipPosition {
                x: mix(self.position.x, other.position.x),
                y: mix(self.position.y, other.position.y),
                z: mix(self.position.z, other.position.z),
                w: mix(self.position.w, other.position.w),
            },
            uv: [mix(self.uv[0], other.uv[0]), mix(self.uv[1], other.uv[1])],
            color: [
                mix(self.color[0], other.color[0]),
                mix(self.color[1], other.color[1]),
                mix(self.color[2], other.color[2]),
            ],
        }
    }
}

fn clip_component(position: camera::ClipPosition, component: usize) -> i64 {
    match component {
        0 => position.x,
        1 => position.y,
        2 => position.z,
        _ => unreachable!("clip component is X, Y, or Z"),
    }
}

fn force_clip_plane(mut vertex: ClipVertex, component: usize, sign: i64) -> ClipVertex {
    let value = sign * vertex.position.w;
    match component {
        0 => vertex.position.x = value,
        1 => vertex.position.y = value,
        2 => vertex.position.z = value,
        _ => unreachable!("clip component is X, Y, or Z"),
    }
    vertex
}

fn clip_plane(vertices: &[ClipVertex], component: usize, sign: i64) -> Vec<ClipVertex> {
    let mut output = Vec::with_capacity(vertices.len() + 2);
    let Some(mut previous) = vertices.last().copied() else {
        return output;
    };
    let distance =
        |vertex: ClipVertex| vertex.position.w - sign * clip_component(vertex.position, component);
    let mut previous_distance = distance(previous);
    for &current in vertices {
        let current_distance = distance(current);
        let previous_inside = previous_distance >= 0;
        let current_inside = current_distance >= 0;
        if previous_inside != current_inside {
            // Anchor the fixed-point interpolation at the outside vertex.
            // Reversing an edge must not reverse its truncation error: two
            // adjoining polygons need exactly the same clipped endpoint.
            let crossing = if previous_inside {
                current.interpolate(
                    previous,
                    current_distance,
                    current_distance - previous_distance,
                )
            } else {
                previous.interpolate(
                    current,
                    previous_distance,
                    previous_distance - current_distance,
                )
            };
            output.push(force_clip_plane(crossing, component, sign));
        }
        if current_inside {
            output.push(current);
        }
        previous = current;
        previous_distance = current_distance;
    }
    output
}

fn culled(vertices: &[ClipVertex], polygon_attr: u32) -> bool {
    let dot = winding(vertices);
    (dot < 0 && polygon_attr & (1 << 7) == 0) || (dot > 0 && polygon_attr & (1 << 6) == 0)
}

fn winding(vertices: &[ClipVertex]) -> i8 {
    let [v0, v1, v2, ..] = vertices else {
        return 0;
    };
    let p0 = v0.position;
    let p1 = v1.position;
    let p2 = v2.position;
    let normal_x = i128::from(p0.y - p1.y) * i128::from(p2.w - p1.w)
        - i128::from(p0.w - p1.w) * i128::from(p2.y - p1.y);
    let normal_y = i128::from(p0.w - p1.w) * i128::from(p2.x - p1.x)
        - i128::from(p0.x - p1.x) * i128::from(p2.w - p1.w);
    let normal_z = i128::from(p0.x - p1.x) * i128::from(p2.y - p1.y)
        - i128::from(p0.y - p1.y) * i128::from(p2.x - p1.x);
    let dot =
        i128::from(p1.x) * normal_x + i128::from(p1.y) * normal_y + i128::from(p1.w) * normal_z;
    dot.signum() as i8
}

fn clip_and_project(
    camera: &Camera,
    vertices: &[Vertex],
    polygon_attr: u32,
) -> Option<Vec<GxProjected>> {
    let clipped: Vec<_> = vertices
        .iter()
        .map(|vertex| ClipVertex {
            position: camera.to_clip_fixed(vertex.position),
            uv: vertex.uv.map(i64::from),
            color: [
                (i64::from(vertex.color & 31) << 12) + 0xFFF,
                (i64::from((vertex.color >> 5) & 31) << 12) + 0xFFF,
                (i64::from((vertex.color >> 10) & 31) << 12) + 0xFFF,
            ],
        })
        .collect();
    let winding = if winding(&clipped) <= 0 { -1 } else { 1 };
    let clipped = clip_vertices(clipped, polygon_attr)?;
    clipped
        .into_iter()
        .map(|vertex| {
            let final_color = vertex.color.map(|channel| {
                let integer = channel >> 12;
                if integer == 0 {
                    0
                } else {
                    (integer << 4) + 0xF
                }
            });
            Projected::from_clip(vertex.position, vertex.uv, final_color).map(|mut projected| {
                projected.0.winding = winding;
                projected.into()
            })
        })
        .collect()
}

fn clip_vertices(mut clipped: Vec<ClipVertex>, polygon_attr: u32) -> Option<Vec<ClipVertex>> {
    if culled(&clipped, polygon_attr) {
        return None;
    }
    // GX far-plane-intersection mode: without bit 12, any vertex beyond
    // +Z rejects the whole polygon instead of producing an intersection.
    if polygon_attr & (1 << 12) == 0
        && clipped
            .iter()
            .any(|vertex| vertex.position.z > vertex.position.w)
    {
        return None;
    }
    // The geometry engine clips Z, then Y, then X, positive before negative.
    for component in [2, 1, 0] {
        clipped = clip_plane(&clipped, component, 1);
        clipped = clip_plane(&clipped, component, -1);
        if clipped.len() < 3 {
            return None;
        }
    }
    Some(clipped)
}

/// A fixed-point vertex after the DS viewport transform.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Projected(GxProjected);

impl From<Projected> for GxProjected {
    fn from(value: Projected) -> Self {
        value.0
    }
}

impl Projected {
    pub(crate) fn from_clip(
        clip: camera::ClipPosition,
        uv: [i64; 2],
        color: [i64; 3],
    ) -> Option<Self> {
        let screen = Camera::screen_fixed(clip)?;
        Some(Self(GxProjected {
            x: screen.x,
            y: screen.y,
            depth: screen.depth,
            w: screen.w,
            uv,
            color,
            winding: 0,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use apricorn_core::{
        field::model::{Primitive, Texture},
        formats::TexFmt,
    };
    use std::sync::Arc;

    #[test]
    fn clipped_shared_edge_is_independent_of_traversal_direction() {
        // The nonintegral 7/12 intersection exposes truncation anchored
        // at different ends. Test every clipping plane with attributes.
        for component in 0..3 {
            for sign in [-1, 1] {
                let make = |distance: i64, uv: i64| {
                    let mut position = camera::ClipPosition {
                        x: 0,
                        y: 0,
                        z: 0,
                        w: 16,
                    };
                    match component {
                        0 => position.x = sign * distance,
                        1 => position.y = sign * distance,
                        _ => position.z = sign * distance,
                    }
                    ClipVertex {
                        position,
                        uv: [uv, -uv],
                        color: [uv + 31; 3],
                    }
                };
                let a = make(23, 17);
                let b = make(11, -8);
                let forward = clip_plane(&[a, b], component, sign);
                let reverse = clip_plane(&[b, a], component, sign);
                let on_plane =
                    |v: &&ClipVertex| clip_component(v.position, component) == sign * v.position.w;
                let crossing = forward.iter().find(on_plane).unwrap();
                assert_eq!(Some(crossing), reverse.iter().find(on_plane));
                assert_eq!(crossing.uv, [3, -3]);
            }
        }
    }

    #[test]
    fn bedroom_roof_geometry_matches_captured_oracle() {
        let path = std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../hg_usa.nds"));
        if !path.exists() {
            return;
        }
        let store = apricorn_core::assets::AssetStore::open(path).unwrap();
        let scene = apricorn_core::field::FieldScene::bedroom(&store, 0).unwrap();
        let camera = Camera::from_preset(&CameraPreset::INDOOR, tile_position([6, 6]));
        let mesh = &scene.meshes[4];
        let camera = camera.with_model(&mesh.gx_transform);
        let vertices: Vec<_> = mesh.primitives[1]
            .vertices()
            .iter()
            .map(|v| Vertex {
                position: v.gx_position,
                ..*v
            })
            .collect();
        let output = clip_and_project(&camera, &vertices, mesh.polygon_attr).unwrap();
        let mut actual: Vec<_> = output.iter().map(|v| (v.x, v.y, v.depth, v.uv)).collect();
        actual.sort();
        let mut expected = vec![
            (136, 12, 14163456, [155, -219]),
            (136, 0, 14272000, [155, -238]),
            (103, 0, 14272000, [90, -238]),
            (104, 12, 14163456, [90, -219]),
        ];
        expected.sort();
        assert_eq!(actual, expected);
    }

    fn vertex(position: [i32; 3], uv: [i16; 2], color: u16) -> Vertex {
        Vertex {
            position,
            gx_position: position,
            uv,
            color,
            normal: None,
        }
    }

    /// A ground quad around `centre` (fx32), `half` units to each
    /// side, in one mesh.
    fn ground(centre: [i32; 3], half: i32, texture: Option<Arc<Texture>>, alpha: u8) -> Mesh {
        let h = half * FX32_ONE;
        let (cx, cz) = (centre[0], centre[2]);
        let corner = |dx: i32, dz: i32, u: i16, v: i16| {
            vertex([cx + dx, centre[1], cz + dz], [u * 16, v * 16], 0x7FFF)
        };
        let a = corner(-h, -h, 0, 0);
        let b = corner(h, -h, 1, 0);
        let c = corner(h, h, 1, 1);
        let d = corner(-h, h, 0, 1);
        Mesh {
            primitives: vec![Primitive::Quad([a, b, c, d])],
            gx_transform: apricorn_core::field::model::GxTransform::IDENTITY,
            texture,
            texture_flags: 0,
            alpha,
            polygon_attr: (u32::from(alpha) << 16) | (3 << 6),
            diffuse_ambient: 0x7FFF,
            specular_emission: 0,
        }
    }

    fn solid(color: [u8; 4]) -> Arc<Texture> {
        Arc::new(Texture {
            width: 1,
            height: 1,
            pixels: vec![color],
            format: TexFmt::Pltt256,
            color0_transparent: false,
            raw: None,
        })
    }

    fn buffers() -> (Vec<[u8; 4]>, Vec<f64>) {
        (vec![[0; 4]; WIDTH * HEIGHT], vec![0.0; WIDTH * HEIGHT])
    }

    #[test]
    fn clear_pixels_are_transparent_and_covered_ones_opaque() {
        let target = tile_position([0, 0]);
        let meshes = [ground(target, 8, Some(solid([200, 100, 50, 255])), 31)];
        let view = SceneView {
            meshes: &meshes,
            land_mesh_count: meshes.len(),
            seal_land_cracks: false,
            billboards: Vec::new(),
            camera: Camera::from_preset(&CameraPreset::INDOOR, target),
            lighting: None,
        };
        let (mut out, mut depth) = buffers();
        render_view(&view, &mut out, &mut depth);
        // The 16-unit tile is centred on the screen and ≈16 px wide.
        assert_eq!(out[96 * WIDTH + 128], [199, 101, 52, 255]);
        assert_eq!(out[0], [0, 0, 0, 0], "uncovered corner stays clear");
        assert!(depth[96 * WIDTH + 128].is_finite());
        assert!(depth[0].is_infinite());
        let covered = out.iter().filter(|p| p[3] != 0).count();
        // 16 wide × (16 · sin(pitch) ≈ 12) tall, give or take an edge.
        assert!((150..=230).contains(&covered), "covered {covered}");
    }

    #[test]
    fn shared_edges_draw_translucent_quads_once() {
        // A translucent quad over the clear colour lands with its own
        // alpha and no double-blended diagonal.
        let target = tile_position([0, 0]);
        let meshes = [ground(target, 40, Some(solid([255, 255, 255, 255])), 15)];
        let view = SceneView {
            meshes: &meshes,
            land_mesh_count: meshes.len(),
            seal_land_cracks: false,
            billboards: Vec::new(),
            camera: Camera::from_preset(&CameraPreset::INDOOR, target),
            lighting: None,
        };
        let (mut out, mut depth) = buffers();
        render_view(&view, &mut out, &mut depth);
        let alpha = (255u16 * 15 / 31) as u8;
        for y in 90..102 {
            for x in 120..136 {
                assert_eq!(out[y * WIDTH + x], [255, 255, 255, alpha], "({x}, {y})");
            }
        }
        assert!(
            depth[96 * WIDTH + 128].is_infinite(),
            "translucent writes no depth"
        );
    }

    #[test]
    fn billboard_stands_on_its_anchor_and_wins_the_feet_row() {
        let target = tile_position([0, 0]);
        let floor = ground(target, 100, Some(solid([10, 20, 30, 255])), 31);
        let sprite = Texture {
            width: 32,
            height: 32,
            pixels: (0..32 * 32)
                .map(|i| {
                    if i / 32 < 16 {
                        [255, 0, 0, 255]
                    } else {
                        [0, 255, 0, 255]
                    }
                })
                .collect(),
            format: TexFmt::Pltt256,
            color0_transparent: false,
            raw: None,
        };
        let meshes = [floor];
        let view = SceneView {
            meshes: &meshes,
            land_mesh_count: meshes.len(),
            seal_land_cracks: false,
            billboards: vec![BillboardView {
                texture: &sprite,
                rect: (0, 0, 32, 32),
                world_pos: target,
                size_px: (32, 32),
                mirrored: false,
            }],
            camera: Camera::from_preset(&CameraPreset::INDOOR, target),
            lighting: None,
        };
        let (mut out, mut depth) = buffers();
        render_view(&view, &mut out, &mut depth);
        // The anchor projects to (128, 96): the quad spans x 112..144
        // and rises 32 px from y = 96 — bottom half green, top half red.
        assert_eq!(out[95 * WIDTH + 128][..3], [0, 255, 0]);
        assert_eq!(out[70 * WIDTH + 128][..3], [255, 0, 0]);
        assert_eq!(
            out[97 * WIDTH + 128][..3],
            [12, 20, 36],
            "floor below the feet"
        );
        assert_eq!(
            out[80 * WIDTH + 100][..3],
            [12, 20, 36],
            "floor beside the quad"
        );
        assert_eq!(out[80 * WIDTH + 150][..3], [12, 20, 36]);
        // The feet row is nearer than the floor there: the bias.
        let floor_depth = depth[97 * WIDTH + 128];
        assert!(depth[95 * WIDTH + 128] < floor_depth);
    }

    #[test]
    fn texture_repeat_and_flip_follow_teximage_param() {
        use crate::gx3d::texture_coordinate;
        assert_eq!(texture_coordinate(5 * 16, 4, false, false), 3, "clamp");
        assert_eq!(texture_coordinate(-16, 4, false, false), 0);
        assert_eq!(texture_coordinate(5 * 16, 4, true, false), 1, "repeat");
        assert_eq!(
            texture_coordinate(5 * 16, 4, true, true),
            2,
            "flip mirrors odd tiles"
        );
        assert_eq!(texture_coordinate(-16, 4, true, true), 0);
    }

    #[test]
    fn gx_lighting_keeps_emission_and_ambient_accumulator_precision() {
        let target = tile_position([0, 0]);
        let camera = Camera::from_preset(&CameraPreset::INDOOR, target);
        let mut mesh = ground(target, 1, None, 31);
        let mut lighting = ModelLighting {
            emission: 1 | (2 << 5) | (3 << 10),
            ..ModelLighting::default()
        };
        assert_eq!(
            light_vertex(
                &mesh,
                &camera,
                &Gx3dFrame::default(),
                lighting,
                [0, 0, 0x1ff]
            ),
            lighting.emission
        );

        mesh.polygon_attr |= 1;
        lighting.emission = 0;
        lighting.ambient = 16 | (8 << 5) | (4 << 10);
        lighting.light_colors[0] = 0x7fff;
        assert_eq!(
            light_vertex(
                &mesh,
                &camera,
                &Gx3dFrame::default(),
                lighting,
                [0, 0, 0x1ff]
            ),
            15 | (7 << 5) | (3 << 10),
            "ambient multiplies in the GX 14-fractional-bit accumulator"
        );
    }

    fn clip_vertex(x: i64, y: i64, z: i64, w: i64) -> ClipVertex {
        ClipVertex {
            position: camera::ClipPosition { x, y, z, w },
            uv: [x, y],
            color: [1, 2, 3],
        }
    }

    #[test]
    fn homogeneous_clipping_keeps_intersections_on_all_six_planes() {
        let polygon = vec![
            clip_vertex(-5, -5, 0, 10),
            clip_vertex(15, -5, 0, 10),
            clip_vertex(15, 5, 0, 10),
            clip_vertex(-5, 5, 0, 10),
        ];
        let clipped = clip_vertices(polygon, 3 << 6).expect("partly visible polygon");
        assert!(clipped.len() >= 4);
        assert!(clipped.iter().all(|vertex| {
            let p = vertex.position;
            p.x.abs() <= p.w && p.y.abs() <= p.w && p.z.abs() <= p.w
        }));
        assert!(
            clipped
                .iter()
                .any(|vertex| vertex.position.x == vertex.position.w)
        );
    }

    #[test]
    fn far_plane_intersection_requires_polygon_attribute_bit_12() {
        let polygon = vec![
            clip_vertex(-5, -5, 0, 10),
            clip_vertex(5, -5, 0, 10),
            clip_vertex(0, 5, 20, 10),
        ];
        assert!(clip_vertices(polygon.clone(), 3 << 6).is_none());
        assert!(clip_vertices(polygon, (3 << 6) | (1 << 12)).is_some());
    }

    #[test]
    fn homogeneous_face_culling_uses_front_and_back_enable_bits() {
        let front = vec![
            clip_vertex(-5, -5, 0, 10),
            clip_vertex(5, -5, 0, 10),
            clip_vertex(0, 5, 0, 10),
        ];
        assert!(!culled(&front, 1 << 7));
        assert!(culled(&front, 1 << 6));
        assert!(!culled(&front, 3 << 6));
        assert!(culled(&front, 0));
    }
}

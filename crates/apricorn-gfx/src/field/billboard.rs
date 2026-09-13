//! Map-object billboards — the `mmodel` quads of `a/0/8/1`.
//!
//! Every overworld character is one shared quad model with a
//! per-character NSBTX (`asm/overlay_01_sprite_data.s:436`
//! `ov01_022074A8` maps sprite → mmodel texture and packs the size
//! class in bits 10–15 of its third u16; `sub_021FA248`,
//! `asm/overlay_01_021F944C.s:2015-2045`, resolves that class through
//! `ov01_02207318`). The standard class is `mmdl_m32x32` (NARC member
//! 266): one `pPlane1` node, and an SBC program of
//! `NODEDESC, NODE, BB, POSSCALE, MAT, SHP, POSSCALE, RET` — opcode 7
//! is `NNS_G3D_SBC_BB`, the *full* billboard, which keeps the node's
//! camera-space translation and scale and replaces its rotation with
//! the identity in camera space (local +x is screen right, +y screen
//! up). Its quad is `x ∈ [−16, 16], y ∈ [0, 32], z = 0` with UVs
//! `(0, 32)` at the bottom-left and `(32, 0)` at the top-right (both
//! read from the ROM's display list — `posScale` 8 over one
//! `BEGIN_VTXS` primitive of type 1, `GX_BEGIN_QUADS`, whose four
//! vertices `VTX_10 (−2, 0, 0)`, `VTX_XY (2, 0)`, `(2, 4)`, `(−2, 4)`
//! run around the rectangle in order, each preceded by its
//! `TEXCOORD`; one quad, no triangles, no strip): the model is
//! anchored at the character's **feet** and stands 32 units tall,
//! which the field presets scale to ≈1 px per unit at the target
//! depth. The 16×16 class (member 267, `posScale` 4, `x ∈ [−8, 8]`,
//! `y ∈ [0, 16]`, `v = 16` at the bottom) and the 64×64 class are the
//! same shape at their sizes.
//!
//! The quad is drawn by `BillboardLists_Draw` (`asm/unk_02023694.s:171`
//! → `sub_02023950` → `GF3dRender_DrawModel`) after the map, while the
//! projection carries the field's depth bias
//! ([`crate::field::camera::BILLBOARD_BIAS_UNITS`]) so the feet row
//! beats the ground under it.
//!
//! Which texture, direction and walk frame a character shows is the
//! map-object agent's concern: a [`BillboardView`] takes a texture and
//! the texel rectangle to show, and this module places it.

use super::Projected;
use super::camera::Camera;
use apricorn_core::field::model::Texture;

/// One billboard to draw: a texel rectangle of `texture` on a
/// camera-facing quad anchored at `world_pos`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BillboardView<'a> {
    /// The character's decoded NSBTX (or any RGBA8 image).
    pub texture: &'a Texture,
    /// The texel rectangle `(u, v, width, height)` shown — the walk
    /// frame within a strip; the whole texture for a one-frame image.
    pub rect: (u16, u16, u16, u16),
    /// The anchor in fx32 world coordinates: the object's position
    /// vector, the bottom-centre of the quad (its feet).
    pub world_pos: [i32; 3],
    /// The quad's width and height in world units — `(32, 32)` for
    /// `mmdl_m32x32` — which the field cameras scale ≈1:1 to pixels
    /// at the target depth.
    pub size_px: (u16, u16),
    /// Show the rectangle mirrored left-to-right (the `u` texel
    /// coordinates swapped between the quad's left and right edges).
    pub mirrored: bool,
}

impl BillboardView<'_> {
    /// The quad's four corners projected through the biased
    /// projection: bottom-left, bottom-right, top-right, top-left —
    /// `None` when the anchor is behind the eye.
    pub(crate) fn corners(&self, camera: &Camera) -> Option<[Projected; 4]> {
        let anchor = camera.to_camera_fixed(self.world_pos);
        let half_w = i64::from(self.size_px.0) * 4096 / 2;
        let h = i64::from(self.size_px.1) * 4096;
        let (u, v, w, rh) = (
            i64::from(self.rect.0) * 16,
            i64::from(self.rect.1) * 16,
            i64::from(self.rect.2) * 16,
            i64::from(self.rect.3) * 16,
        );
        // NNSi_G3dFuncSbc_BB: the local axes become the camera's, the
        // translation stays — so the quad lives in camera space at the
        // anchor's depth. A mirrored view swaps the left and right
        // texel columns.
        let (u_left, u_right) = if self.mirrored {
            (u + w, u)
        } else {
            (u, u + w)
        };
        let corners = [
            ([anchor[0] - half_w, anchor[1], anchor[2]], [u_left, v + rh]),
            (
                [anchor[0] + half_w, anchor[1], anchor[2]],
                [u_right, v + rh],
            ),
            ([anchor[0] + half_w, anchor[1] + h, anchor[2]], [u_right, v]),
            ([anchor[0] - half_w, anchor[1] + h, anchor[2]], [u_left, v]),
        ];
        let mut out = [Projected::default(); 4];
        for (slot, (point, uv)) in out.iter_mut().zip(corners) {
            let clip = camera.camera_to_clip_fixed(point, true);
            *slot = Projected::from_clip(clip, uv, [0x1FF; 3])?;
        }
        Some(out)
    }
}

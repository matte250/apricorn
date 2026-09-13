//! # apricorn-gfx
//!
//! Deterministic CPU rasterizer: [`LogicalFrame`]s to RGBA buffers.
//!
//! The renderer architecture (PLAN.md Phase 3) is *CPU rasterize,
//! wgpu present*: this crate turns one logical frame into two
//! 256×192 RGBA8 screens with plain integer arithmetic, and the
//! presenter (apricorn-desktop, step 8) only uploads and draws them.
//! GPU compositing can come later without changing this contract.
//!
//! Everything here is a pure function of the frame and the asset
//! source — no time, no state, and integer arithmetic throughout the
//! 2D path — so the same frame hashes the same pixels in the dump CLI,
//! the golden tests, and the harness, forever. The field's 3D layer is
//! handled by [`gx3d`], whose viewport,
//! scan-conversion, depth and attribute interpolation are integer-only.

#![deny(missing_docs)]

pub mod field;
pub mod gx3d;
pub mod raster;
mod sprites;

pub use field::{BillboardView, Camera, CameraPreset, SceneView, render_view};
pub use gx3d::{Gx3dBuffer, Gx3dFrame};
pub use raster::{AssetSource, ScreenBuffer, render};

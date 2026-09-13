//! Static NSBMD subset for field geometry: NNS resource dictionaries,
//! node SRT data (translation, full or pivot-compressed rotation, scale —
//! NNS `NNSG3dResNodeData`), the SBC node/matrix/material/shape program
//! with POSSCALE tracking, material bindings and packed GX vertex
//! streams. Models parse into model space once ([`parse`]) and are
//! placed per instance ([`place`]) with the same fx32 matrix math the
//! hardware pipeline uses (64-bit products, arithmetic shift by 12).
//! Unsupported opcodes fail explicitly, never draw junk.
//!
//! Row/column conventions: the NSBMD stores NNS row-vector matrices
//! (`v' = v · M`, `_ij` = row `i` column `j`); this module keeps every
//! matrix in the GX column-vector form (`v' = M · v + t`), so a stored
//! rotation is transposed on read and `G3_MultMtx43(local)` becomes
//! `current = current × local` (apply `local` first).
use crate::{
    formats::{Btx, TexFmt},
    nds::{NdsError, u16le, u32le},
};
use std::collections::HashMap;
use std::sync::Arc;

/// Decoded NNS texture and its bound palette.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Texture {
    /// Width in pixels.
    pub width: usize,
    /// Height in pixels.
    pub height: usize,
    /// Row-major RGBA8 pixels.
    pub pixels: Vec<[u8; 4]>,
    /// Original GX texture format; retained for exact alpha expansion
    /// and opaque/translucent polygon classification.
    pub format: TexFmt,
    /// Original `TEXIMAGE_PARAM` color-zero transparency flag.
    pub color0_transparent: bool,
    /// Original texels and palette. ROM textures retain this so the GX
    /// sampler never reconstructs native values from RGBA8 review data.
    pub raw: Option<RawTexture>,
}

/// Native texture payload retained through model parsing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawTexture {
    /// Packed texels in the texture format's native bit layout.
    pub texels: Vec<u8>,
    /// Palette entries in native BGR555 form.
    pub palette: Vec<u16>,
}
/// A GX vertex after node and placement transforms.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Vertex {
    /// Coordinates with twelve fractional bits (model space from
    /// [`parse`], world space after [`place`]).
    pub position: [i32; 3],
    /// Original GX vertex before POSSCALE and node/base transforms.
    /// The renderer combines this with [`Mesh::gx_transform`] so matrix
    /// quantization happens at the same boundary as on the DS.
    pub gx_position: [i32; 3],
    /// Texture coordinates with four fractional bits.
    pub uv: [i16; 2],
    /// Vertex color in BGR555.
    pub color: u16,
    /// Current GX normal in signed 1.9 form after the model's vector
    /// matrix, or `None` when an explicit COLOR command owns the value.
    pub normal: Option<[i16; 3]>,
}

/// One polygon emitted by a GX primitive stream.
///
/// Quads stay quads until the rasterizer.  The Nintendo DS interpolates
/// and fills a four-vertex polygon as one polygon; eagerly splitting it
/// into two host triangles introduces a synthetic diagonal and produces
/// camera-dependent texel and coverage changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Primitive {
    /// A three-vertex polygon.
    Triangle([Vertex; 3]),
    /// A four-vertex polygon.
    Quad([Vertex; 4]),
}

impl Primitive {
    /// Visits every vertex in primitive order.
    #[must_use]
    pub fn vertices(&self) -> &[Vertex] {
        match self {
            Self::Triangle(vertices) => vertices,
            Self::Quad(vertices) => vertices,
        }
    }

    fn map_vertices(self, mut transform: impl FnMut(Vertex) -> Vertex) -> Self {
        match self {
            Self::Triangle(vertices) => Self::Triangle(vertices.map(&mut transform)),
            Self::Quad(vertices) => Self::Quad(vertices.map(&mut transform)),
        }
    }
}

/// A single material/shape draw from the model's SBC program.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mesh {
    /// GX polygons in draw order. Triangle and quad strips are expanded
    /// into their constituent polygons, but quads are never triangulated.
    pub primitives: Vec<Primitive>,
    /// Model/node/base transform retained until GX submission.
    pub gx_transform: GxTransform,
    /// Bound texture; absent for solid-color shapes such as shadows.
    pub texture: Option<Arc<Texture>>,
    /// Material TEXIMAGE_PARAM, including repeat/flip bits.
    pub texture_flags: u32,
    /// Polygon alpha, 0–31.
    pub alpha: u8,
    /// Raw GX `POLYGON_ATTR` material register (culling, depth mode,
    /// fog, alpha, polygon id, and lighting mask).
    pub polygon_attr: u32,
    /// Raw GX `DIF_AMB` material word.
    pub diffuse_ambient: u32,
    /// Raw GX `SPE_EMI` material word.
    pub specular_emission: u32,
}

/// One unit in fx32 (twelve fractional bits).
pub const FX32_ONE: i32 = 4096;

/// fx32 product: 64-bit intermediate, arithmetic shift by 12 — the
/// hardware/SDK rounding (`FX_Mul`, `asr`), never a rounded division.
#[must_use]
pub const fn fx_mul(a: i32, b: i32) -> i32 {
    ((a as i64 * b as i64) >> 12) as i32
}

/// A rigid placement of a model into the world:
/// `world = translation + rotation · (scale ⊙ v)` — the NNS global base
/// matrix `NNS_G3dGlbSetBaseScale/Rot/Trans` order (scale, then rotation,
/// then translation), applied outside the model's own node matrices.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Placement {
    /// World translation, fx32.
    pub translation: [i32; 3],
    /// Rotation, row-major column-vector fx32 (`v' = R · v`).
    pub rotation: [[i32; 3]; 3],
    /// Per-axis scale, fx32.
    pub scale: [i32; 3],
}

impl Placement {
    /// No rotation, unit scale, zero translation.
    pub const IDENTITY: Self = Self {
        translation: [0; 3],
        rotation: [[FX32_ONE, 0, 0], [0, FX32_ONE, 0], [0, 0, FX32_ONE]],
        scale: [FX32_ONE; 3],
    };

    /// A pure translation.
    #[must_use]
    pub const fn at(translation: [i32; 3]) -> Self {
        Self {
            translation,
            ..Self::IDENTITY
        }
    }

    /// Transforms one model-space point.
    #[must_use]
    pub fn apply(&self, v: [i32; 3]) -> [i32; 3] {
        let s = [
            fx_mul(v[0], self.scale[0]),
            fx_mul(v[1], self.scale[1]),
            fx_mul(v[2], self.scale[2]),
        ];
        let mut out = self.translation;
        for (r, row) in self.rotation.iter().enumerate() {
            let acc = (0..3).map(|c| row[c] as i64 * s[c] as i64).sum::<i64>();
            out[r] += (acc >> 12) as i32;
        }
        out
    }
}

/// Copies `meshes` with every vertex run through `placement`.
#[must_use]
pub fn place(meshes: &[Mesh], placement: &Placement) -> Vec<Mesh> {
    let placement_transform = GxTransform {
        m: std::array::from_fn(|row| std::array::from_fn(|column| {
            fx_mul(placement.rotation[row][column], placement.scale[column])
        })),
        t: placement.translation,
    };
    meshes
        .iter()
        .map(|mesh| Mesh {
            primitives: mesh
                .primitives
                .iter()
                .copied()
                .map(|primitive| {
                    primitive.map_vertices(|vertex| Vertex {
                        position: placement.apply(vertex.position),
                        normal: vertex.normal.map(|normal| {
                            std::array::from_fn(|row| {
                                let value = (0..3)
                                    .map(|column| {
                                        i64::from(placement.rotation[row][column])
                                            * i64::from(normal[column])
                                    })
                                    .sum::<i64>()
                                    >> 12;
                                value.clamp(-512, 511) as i16
                            })
                        }),
                        ..vertex
                    })
                })
                .collect(),
            gx_transform: placement_transform.mul(&mesh.gx_transform),
            texture: mesh.texture.clone(),
            texture_flags: mesh.texture_flags,
            alpha: mesh.alpha,
            polygon_attr: mesh.polygon_attr,
            diffuse_ambient: mesh.diffuse_ambient,
            specular_emission: mesh.specular_emission,
        })
        .collect()
}

/// 4x3 fx32 affine matrix in column-vector form.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GxTransform {
    m: [[i32; 3]; 3],
    t: [i32; 3],
}

impl GxTransform {
    /// Identity transform.
    pub const IDENTITY: Self = Self {
        m: [[FX32_ONE, 0, 0], [0, FX32_ONE, 0], [0, 0, FX32_ONE]],
        t: [0; 3],
    };

    fn apply(&self, v: [i32; 3]) -> [i32; 3] {
        let mut out = self.t;
        for r in 0..3 {
            let acc = (0..3)
                .map(|c| self.m[r][c] as i64 * v[c] as i64)
                .sum::<i64>();
            out[r] += (acc >> 12) as i32;
        }
        out
    }

    /// `self × rhs`: applies `rhs` first, then `self` — `G3_MultMtx43`.
    fn mul(&self, rhs: &Self) -> Self {
        let mut m = [[0i32; 3]; 3];
        for r in 0..3 {
            for c in 0..3 {
                let acc = (0..3)
                    .map(|k| self.m[r][k] as i64 * rhs.m[k][c] as i64)
                    .sum::<i64>();
                m[r][c] = (acc >> 12) as i32;
            }
        }
        Self {
            m,
            t: self.apply(rhs.t),
        }
    }

    /// Column-vector rotation/scale matrix.
    #[must_use]
    pub const fn matrix(&self) -> [[i32; 3]; 3] {
        self.m
    }

    /// Fx32 translation.
    #[must_use]
    pub const fn translation(&self) -> [i32; 3] {
        self.t
    }
}

fn invalid(what: &'static str) -> NdsError {
    NdsError::Invalid { what }
}
fn slice(b: &[u8], p: usize, n: usize) -> Result<&[u8], NdsError> {
    b.get(p..p.checked_add(n).ok_or(invalid("model length"))?)
        .ok_or(NdsError::Truncated {
            what: "model",
            need: p.saturating_add(n),
            got: b.len(),
        })
}
fn dict(b: &[u8], p: usize) -> Result<Vec<(&str, &[u8])>, NdsError> {
    let head = slice(b, p, 8)?;
    let e = p + u16le(head, 6)? as usize;
    let size = u16le(b, e)? as usize;
    let names = e + u16le(b, e + 2)? as usize;
    (0..head[1] as usize)
        .map(|i| {
            let name = slice(b, names + 16 * i, 16)?;
            let name = name.split(|&v| v == 0).next().unwrap();
            Ok((
                std::str::from_utf8(name).map_err(|_| invalid("model name"))?,
                slice(b, e + 4 + i * size, size)?,
            ))
        })
        .collect()
}
/// Decodes one NSBTX texture through one of its palettes into RGBA8.
///
/// Palette index 0 is transparent when the texture's `color0` flag says
/// so; A3I5/A5I3 carry their own alpha. Public so the field system can
/// decode a map object's walk frames (`a/0/8/1` members hold one
/// texture per frame) the same way materials are decoded here.
///
/// # Errors
/// Returns an [`NdsError`] when the texture or palette name is not in
/// the archive or the pixel data is short.
pub fn decode_texture(btx: &Btx<'_>, name: &str, palette: &str) -> Result<Texture, NdsError> {
    let tex = btx
        .texture_by_name(name)
        .ok_or(invalid("material texture binding"))?;
    let pal = btx
        .palette_by_name(palette)
        .ok_or(invalid("material palette binding"))?;
    let pixels = (0..tex.width() as usize * tex.height() as usize)
        .map(|i| {
            let byte = tex
                .data()
                .get(match tex.fmt() {
                    TexFmt::Pltt4 => i / 4,
                    TexFmt::Pltt16 => i / 2,
                    _ => i,
                })
                .copied()
                .ok_or(invalid("texture pixels"))?;
            let (index, alpha) = match tex.fmt() {
                TexFmt::Pltt4 => ((byte >> (i % 4 * 2)) & 3, 255),
                TexFmt::Pltt16 => ((byte >> (i % 2 * 4)) & 15, 255),
                TexFmt::Pltt256 => (byte, 255),
                TexFmt::A3i5 => (byte & 31, ((byte >> 5) as u16 * 255 / 7) as u8),
                TexFmt::A5i3 => (byte & 7, ((byte >> 3) as u16 * 255 / 31) as u8),
            };
            let rgb = u16le(pal.data(), index as usize * 2)?;
            let a = if index == 0 && tex.color0_transparent()
                && matches!(tex.fmt(), TexFmt::Pltt4 | TexFmt::Pltt16 | TexFmt::Pltt256)
            {
                0
            } else {
                alpha
            };
            Ok([
                expand(rgb & 31),
                expand((rgb >> 5) & 31),
                expand((rgb >> 10) & 31),
                a,
            ])
        })
        .collect::<Result<Vec<_>, NdsError>>()?;
    let palette_entries = match tex.fmt() {
        TexFmt::Pltt4 => 4,
        TexFmt::Pltt16 => 16,
        TexFmt::Pltt256 => 256,
        TexFmt::A3i5 => 32,
        TexFmt::A5i3 => 8,
    };
    let native_palette = (0..palette_entries)
        .map(|index| u16le(pal.data(), index * 2))
        .collect::<Result<Vec<_>, NdsError>>()?;
    Ok(Texture {
        width: tex.width() as usize,
        height: tex.height() as usize,
        pixels,
        format: tex.fmt(),
        color0_transparent: tex.color0_transparent(),
        raw: Some(RawTexture {
            texels: tex.data().to_vec(),
            palette: native_palette,
        }),
    })
}
fn expand(v: u16) -> u8 {
    ((v << 3) | (v >> 2)) as u8
}

/// Sign-extends an fx16.
fn fx16(b: &[u8], p: usize) -> Result<i32, NdsError> {
    Ok(u16le(b, p)? as i16 as i32)
}

/// Decodes one node's SRT record (`NNSG3dResNodeData` at `p`):
/// `u16 flag; fx16 _00;` then, gated by the flag's low bits (set = the
/// component is trivial and absent), `VecFx32 trans`, the rotation —
/// `fx16 A, B` when bit 3 marks a pivot, else the remaining eight fx16
/// row-major elements `_01.._22` — and `VecFx32 scale, invScale`. Pivot
/// rotations put ±1 at row/column `idx = flag>>4 & 15` (bit 8 negates)
/// and `[A B; C D]` on the other rows × columns, `C = ±B` (bit 9),
/// `D = ±A` (bit 10). Retail bm_field prop 27's node 1 is the worked
/// example: flag `0xFA4C`, a 40° pivot-Y rotation with `_00 == A`.
fn node_matrix(m: &[u8], p: usize) -> Result<GxTransform, NdsError> {
    let flag = u16le(m, p)?;
    let m00 = fx16(m, p + 2)?;
    let mut q = p + 4;
    let mut t = [0i32; 3];
    if flag & 1 == 0 {
        for axis in &mut t {
            *axis = u32le(m, q)? as i32;
            q += 4;
        }
    }
    // Row-vector rotation as stored (`rows[i][j]` = `_ij`).
    let mut rows = [[FX32_ONE, 0, 0], [0, FX32_ONE, 0], [0, 0, FX32_ONE]];
    if flag & 2 == 0 {
        if flag & 8 != 0 {
            let a = fx16(m, q)?;
            let b = fx16(m, q + 2)?;
            q += 4;
            let idx = ((flag >> 4) & 15) as usize;
            if idx > 8 {
                return Err(invalid("model node pivot index"));
            }
            let (pr, pc) = (idx / 3, idx % 3);
            let one = if flag & 0x100 != 0 {
                -FX32_ONE
            } else {
                FX32_ONE
            };
            let c = if flag & 0x200 != 0 { -b } else { b };
            let d = if flag & 0x400 != 0 { -a } else { a };
            let others = |i: usize| -> [usize; 2] {
                let mut it = (0..3).filter(|&k| k != i);
                [it.next().unwrap(), it.next().unwrap()]
            };
            let (rs, cs) = (others(pr), others(pc));
            rows = [[0; 3]; 3];
            rows[pr][pc] = one;
            rows[rs[0]][cs[0]] = a;
            rows[rs[0]][cs[1]] = b;
            rows[rs[1]][cs[0]] = c;
            rows[rs[1]][cs[1]] = d;
        } else {
            rows[0][0] = m00;
            let rest = [
                (0, 1),
                (0, 2),
                (1, 0),
                (1, 1),
                (1, 2),
                (2, 0),
                (2, 1),
                (2, 2),
            ];
            for (i, j) in rest {
                rows[i][j] = fx16(m, q)?;
                q += 2;
            }
        }
    }
    let mut s = [FX32_ONE; 3];
    if flag & 4 == 0 {
        for axis in &mut s {
            *axis = u32le(m, q)? as i32;
            q += 4;
        }
        // invScale follows; unused here.
    }
    // Column-vector form of (scale, then rotate): M[r][c] = rows[c][r] * s[c].
    let mut out = [[0i32; 3]; 3];
    for r in 0..3 {
        for c in 0..3 {
            out[r][c] = fx_mul(rows[c][r], s[c]);
        }
    }
    Ok(GxTransform { m: out, t })
}

/// Parses a single-model BMD0 into model space: node SRTs applied
/// through the SBC program, POSSCALE applied to the shapes it brackets,
/// and each material/shape pair decoded into one [`Mesh`] with its
/// texture bound from `tex`. Textures shared within the model decode
/// once.
pub(crate) fn parse(b: &[u8], tex: &Btx<'_>) -> Result<Vec<Mesh>, NdsError> {
    if slice(b, 0, 4)? != b"BMD0" || u32le(b, 8)? as usize != b.len() {
        return Err(invalid("BMD0 header"));
    }
    let block = u32le(b, 16)? as usize;
    let set = slice(b, block, u32le(b, block + 4)? as usize)?;
    if slice(set, 0, 4)? != b"MDL0" {
        return Err(invalid("MDL0 block"));
    }
    let models = dict(set, 8)?;
    if models.len() != 1 {
        return Err(invalid("single field model"));
    }
    let offset = u32le(models[0].1, 0)? as usize;
    let m = slice(set, offset, u32le(set, offset)? as usize)?;
    let nodes = dict(m, 64)?;
    let node_matrices = nodes
        .iter()
        .map(|(_, d)| node_matrix(m, 64 + u32le(d, 0)? as usize))
        .collect::<Result<Vec<_>, _>>()?;
    let pos_scale = u32le(m, 28)? as i32;
    let mat = u32le(m, 8)? as usize;
    let shp = u32le(m, 12)? as usize;
    let materials = dict(m, mat + 4)?;
    let shapes = dict(m, shp)?;
    let mut tex_names = vec![None; materials.len()];
    let mut pal_names = vec![None; materials.len()];
    for (offset, names) in [(0, &mut tex_names), (2, &mut pal_names)] {
        for (name, d) in dict(m, mat + u16le(m, mat + offset)? as usize)? {
            let ids = slice(
                m,
                mat + u16le(d, 0)? as usize,
                *d.get(2).ok_or(invalid("material binding count"))? as usize,
            )?;
            for &id in ids {
                *names
                    .get_mut(id as usize)
                    .ok_or(invalid("material binding index"))? = Some(name);
            }
        }
    }
    let mut textures: HashMap<(&str, &str), Arc<Texture>> = HashMap::new();
    let mut meshes = Vec::new();
    let mut p = u32le(m, 4)? as usize;
    let mut selected = 0;
    let mut current = GxTransform::IDENTITY;
    let mut stack: [Option<GxTransform>; 32] = [None; 32];
    let mut scaled = false;
    let mut visible = true;
    while p < mat {
        let op = m[p];
        p += 1;
        match op {
            0 => {}
            1 => break,
            2 => {
                let args = slice(m, p, 2)?;
                visible = args[1] != 0;
                p += 2;
            }
            3 => {
                let idx = slice(m, p, 1)?[0] as usize;
                p += 1;
                current = stack
                    .get(idx)
                    .copied()
                    .flatten()
                    .ok_or(invalid("SBC matrix restore"))?;
            }
            0x06 | 0x26 | 0x46 | 0x66 => {
                let extra = usize::from(op & 0x20 != 0) + usize::from(op & 0x40 != 0);
                let args = slice(m, p, 3 + extra)?;
                p += 3 + extra;
                let node = args[0] as usize;
                if op & 0x40 != 0 {
                    let idx = args[3 + usize::from(op & 0x20 != 0)] as usize;
                    current = stack
                        .get(idx)
                        .copied()
                        .flatten()
                        .ok_or(invalid("SBC node restore"))?;
                }
                let local = node_matrices.get(node).ok_or(invalid("SBC node index"))?;
                current = current.mul(local);
                if op & 0x20 != 0 {
                    let idx = args[3] as usize;
                    *stack.get_mut(idx).ok_or(invalid("SBC node store"))? = Some(current);
                }
            }
            0x0b => scaled = true,
            0x2b => scaled = false,
            4 | 0x24 | 0x44 => {
                selected = *slice(m, p, 1)?.first().unwrap() as usize;
                p += 1;
            }
            5 => {
                let id = slice(m, p, 1)?[0] as usize;
                p += 1;
                if !visible {
                    continue;
                }
                let (_, d) = materials.get(selected).ok_or(invalid("SBC material"))?;
                let material = mat + u32le(d, 0)? as usize;
                let (_, d) = shapes.get(id).ok_or(invalid("SBC shape"))?;
                let shape = shp + u32le(d, 0)? as usize;
                let dl = slice(
                    m,
                    shape + u32le(m, shape + 8)? as usize,
                    u32le(m, shape + 12)? as usize,
                )?;
                let texture = match (tex_names[selected], pal_names[selected]) {
                    (Some(t), Some(pal)) => Some(match textures.get(&(t, pal)) {
                        Some(cached) => cached.clone(),
                        None => {
                            let decoded = Arc::new(decode_texture(tex, t, pal)?);
                            textures.insert((t, pal), decoded.clone());
                            decoded
                        }
                    }),
                    (None, None) => None,
                    _ => return Err(invalid("incomplete material binding")),
                };
                let polygon_attr = u32le(m, material + 12)?;
                let diffuse_ambient = u32le(m, material + 4)?;
                let specular_emission = u32le(m, material + 8)?;
                meshes.push(Mesh {
                    primitives: display_list(
                        dl,
                        if scaled { pos_scale } else { FX32_ONE },
                        &current,
                        diffuse_ambient as u16 & 32767,
                    )?,
                    gx_transform: GxTransform {
                        m: current.m.map(|row| row.map(|value| {
                            fx_mul(value, if scaled { pos_scale } else { FX32_ONE })
                        })),
                        t: current.t,
                    },
                    texture,
                    texture_flags: u32le(m, material + 20)?,
                    alpha: ((polygon_attr >> 16) & 31) as u8,
                    polygon_attr,
                    diffuse_ambient,
                    specular_emission,
                });
            }
            _ => return Err(invalid("unsupported field SBC opcode")),
        }
    }
    Ok(meshes)
}
fn sign10(v: u32) -> i32 {
    ((v << 22) as i32) >> 22
}
fn display_list(
    b: &[u8],
    scale: i32,
    node: &GxTransform,
    initial: u16,
) -> Result<Vec<Primitive>, NdsError> {
    let mut p = 0;
    let mut position = [0i32; 3];
    let mut uv = [0; 2];
    let mut color = initial;
    let mut normal = None;
    let mut vertices = Vec::new();
    let mut primitives = Vec::new();
    let mut primitive = None;
    while p < b.len() {
        let ops = slice(b, p, 4)?.to_vec();
        p += 4;
        for op in ops {
            let n = match op {
                0 | 0x41 => 0,
                0x23 => 2,
                0x20..=0x22 | 0x24..=0x2b | 0x30..=0x33 | 0x40 => 1,
                0x34 => 32,
                _ => return Err(invalid("unsupported field GX command")),
            };
            let args = slice(b, p, n * 4)?;
            p += n * 4;
            let a = if n > 0 { u32le(args, 0)? } else { 0 };
            match op {
                0 => {}
                0x20 => {
                    color = a as u16 & 32767;
                    normal = None;
                }
                0x21 => {
                    normal = Some([
                        sign10(a) as i16,
                        sign10(a >> 10) as i16,
                        sign10(a >> 20) as i16,
                    ]);
                }
                0x22 => uv = [a as i16, (a >> 16) as i16],
                0x23 => {
                    position = [
                        a as i16 as i32,
                        (a >> 16) as i16 as i32,
                        u32le(args, 4)? as i16 as i32,
                    ];
                }
                0x24 => position = [sign10(a) << 6, sign10(a >> 10) << 6, sign10(a >> 20) << 6],
                0x25 => {
                    position[0] = a as i16 as i32;
                    position[1] = (a >> 16) as i16 as i32;
                }
                0x26 => {
                    position[0] = a as i16 as i32;
                    position[2] = (a >> 16) as i16 as i32;
                }
                0x27 => {
                    position[1] = a as i16 as i32;
                    position[2] = (a >> 16) as i16 as i32;
                }
                0x28 => {
                    for i in 0..3 {
                        position[i] += sign10(a >> (i * 10));
                    }
                }
                // POLYGON_ATTR, TEXIMAGE_PARAM, PLTT_BASE and the material
                // lighting parameters (DIF_AMB .. SHININESS) carry no
                // geometry; the material record already supplies what the
                // rasterizer needs.
                0x29..=0x2b | 0x30..=0x34 => {}
                0x40 => {
                    primitive = Some(a & 3);
                    vertices.clear();
                }
                0x41 => primitive = None,
                _ => unreachable!(),
            }
            if (0x23..=0x28).contains(&op) {
                if primitive.is_none() {
                    return Err(invalid("vertex outside primitive"));
                }
                let local = [
                    fx_mul(position[0], scale),
                    fx_mul(position[1], scale),
                    fx_mul(position[2], scale),
                ];
                vertices.push(Vertex {
                    position: node.apply(local),
                    gx_position: position,
                    uv,
                    color,
                    normal: normal.map(|normal| {
                        std::array::from_fn(|row| {
                            let value = (0..3)
                                .map(|column| i64::from(node.m[row][column]) * i64::from(normal[column]))
                                .sum::<i64>()
                                >> 12;
                            value.clamp(-512, 511) as i16
                        })
                    }),
                });
                let n = vertices.len();
                match primitive.unwrap() {
                    0 if n % 3 == 0 => primitives.push(Primitive::Triangle([
                        vertices[n - 3],
                        vertices[n - 2],
                        vertices[n - 1],
                    ])),
                    1 if n % 4 == 0 => primitives.push(Primitive::Quad([
                        vertices[n - 4],
                        vertices[n - 3],
                        vertices[n - 2],
                        vertices[n - 1],
                    ])),
                    2 if n >= 3 => {
                        let ids = if n % 2 == 1 {
                            [n - 3, n - 2, n - 1]
                        } else {
                            [n - 2, n - 3, n - 1]
                        };
                        primitives.push(Primitive::Triangle(ids.map(|i| vertices[i])));
                    }
                    3 if n >= 4 && n % 2 == 0 => primitives.push(Primitive::Quad([
                        vertices[n - 4],
                        vertices[n - 3],
                        vertices[n - 1],
                        vertices[n - 2],
                    ])),
                    _ => {}
                }
            }
        }
    }
    Ok(primitives)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fx_mul_truncates_like_the_hardware() {
        assert_eq!(fx_mul(FX32_ONE, FX32_ONE), FX32_ONE);
        assert_eq!(fx_mul(-1, FX32_ONE), -1);
        // -0.5 * 0.5 = -0.25 exactly; -1/4096 * 1/4096 rounds toward -inf.
        assert_eq!(fx_mul(-2048, 2048), -1024);
        assert_eq!(fx_mul(-1, 1), -1);
    }

    #[test]
    fn placement_scales_rotates_then_translates() {
        // 90° about Y in column form: x' = z, z' = -x.
        let p = Placement {
            translation: [100, 200, 300],
            rotation: [[0, 0, FX32_ONE], [0, FX32_ONE, 0], [-FX32_ONE, 0, 0]],
            scale: [2 * FX32_ONE, FX32_ONE, FX32_ONE],
        };
        assert_eq!(p.apply([10, 20, 30]), [130, 220, 280]);
        assert_eq!(Placement::IDENTITY.apply([1, 2, 3]), [1, 2, 3]);
        assert_eq!(Placement::at([5, 6, 7]).apply([1, 2, 3]), [6, 8, 10]);
    }

    #[test]
    fn matrix_product_applies_rhs_first() {
        let t = GxTransform {
            t: [FX32_ONE, 0, 0],
            ..GxTransform::IDENTITY
        };
        let r = GxTransform {
            m: [[0, 0, FX32_ONE], [0, FX32_ONE, 0], [-FX32_ONE, 0, 0]],
            t: [0; 3],
        };
        // (r × t)(v) = r(t(v)): translate then rotate.
        let v = [0, 0, 0];
        assert_eq!(r.mul(&t).apply(v), [0, 0, -FX32_ONE]);
        assert_eq!(t.mul(&r).apply(v), [FX32_ONE, 0, 0]);
    }

    #[test]
    fn pivot_node_decodes_the_retail_example() {
        // bm_field 27 node 1: flag 0xFA4C, trans (0, 5.0, 0), A/B = cos/sin 40°.
        let mut rec = vec![0x4C, 0xFA, 0x42, 0x0C];
        rec.extend_from_slice(&0u32.to_le_bytes());
        rec.extend_from_slice(&0x50000u32.to_le_bytes());
        rec.extend_from_slice(&0u32.to_le_bytes());
        rec.extend_from_slice(&[0x42, 0x0C, 0x49, 0x0A]);
        let n = node_matrix(&rec, 0).unwrap();
        assert_eq!(n.t, [0, 0x50000, 0]);
        // Row-vector rows: [[A,0,B],[0,1,0],[-B,0,A]] → column form transposed.
        assert_eq!(n.m[0], [0x0C42, 0, -0x0A49]);
        assert_eq!(n.m[1], [0, FX32_ONE, 0]);
        assert_eq!(n.m[2], [0x0A49, 0, 0x0C42]);
    }

    #[test]
    fn identity_node_is_identity() {
        let rec = [0x07, 0x00, 0x00, 0x10];
        assert_eq!(node_matrix(&rec, 0).unwrap(), GxTransform::IDENTITY);
    }
}

//! Cached native RGB samples for channel previews. Only geometry/view uniforms change while
//! dragging. Unsupported stacks, monitor transforms and devices use the CPU preview oracle.

use std::sync::Arc;

use eframe::{egui_wgpu::RenderState, wgpu};
use photocraft_color::{BlendMode, ColorMode, SampleType};
use photocraft_doc::{Document, Layer, LayerId};
use photocraft_engine::channel_cmds::ChannelView;

use crate::state::TransformSession;

const OUTPUT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
const FLOAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba32Float;
const UNIFORM_BYTES: u64 = 112;

struct Pipelines {
    inputs: wgpu::BindGroupLayout,
    moved: wgpu::BindGroupLayout,
    affine: wgpu::RenderPipeline,
    warp: wgpu::RenderPipeline,
    finish: wgpu::RenderPipeline,
}

pub(crate) struct Preview {
    rs: RenderState,
    pipes: Arc<Pipelines>,
    uniform: wgpu::Buffer,
    inputs: wgpu::BindGroup,
    moved: wgpu::BindGroup,
    moved_view: wgpu::TextureView,
    output_view: wgpu::TextureView,
    _output: wgpu::Texture,
    id: egui::TextureId,
    size: [u32; 2],
    opacity: f32,
    steps: f32,
    fill: [f32; 4],
    selected: bool,
    last: Option<(TransformSession, ChannelView, bool, bool)>,
    vertices: wgpu::Buffer,
    indices: wgpu::Buffer,
}

/// Deliberately narrow: no flattened RGB shortcut may change a mask, blending or ICC result.
fn source(doc: &Document, layer: Option<LayerId>) -> Option<&Layer> {
    if doc.mode != ColorMode::Rgb {
        return None;
    }
    let mut visible = doc.layers.iter().filter(|l| l.visible);
    let l = visible.next()?;
    (visible.next().is_none()
        && Some(l.id) == layer
        && matches!(l.content, photocraft_doc::LayerContent::Raster(_))
        && l.surface().is_some_and(|s| s.format().mode == ColorMode::Rgb && s.format().sample == doc.depth)
        && l.blend == BlendMode::Normal
        && !l.clipped
        && l.mask.is_none()
        && l.vector_mask.is_none()
        && !photocraft_compose::effects::has_effects(l)
        && l.excluded_channels == 0
        && l.blend_if == Default::default()
        && l.advanced == Default::default()
        && l.video.is_none()
        && (0.0..=1.0).contains(&l.opacity)
        && (0.0..=1.0).contains(&l.fill_opacity))
    .then_some(l)
}

impl Preview {
    pub(crate) fn new(gpu: &crate::gpu_canvas::GpuCanvas, doc: &Document, layer: Option<LayerId>, fill: [f32; 4]) -> Option<Self> {
        let l = source(doc, layer)?;
        let surf = l.surface()?;
        let size = [doc.size.width, doc.size.height];
        let rs = gpu.render_state();
        let limit = rs.device.limits().max_texture_dimension_2d;
        // Source, selection, moved component and display: checked before GPU/CPU allocations.
        let bytes = doc.size.area().checked_mul(40)?;
        if !gpu.health().is_ok()
            || size.contains(&0)
            || size.into_iter().any(|n| n > limit)
            || bytes > gpu.memory_budget().unwrap_or(256 * 1024 * 1024).min(512 * 1024 * 1024)
            || fill.iter().any(|v| !v.is_finite() || v.abs() > 65504.0)
            || !rs.adapter.get_texture_format_features(FLOAT).allowed_usages.contains(wgpu::TextureUsages::RENDER_ATTACHMENT)
        {
            return None;
        }
        let pipes = {
            let mut renderer = rs.renderer.write();
            if let Some(p) = renderer.callback_resources.get::<Arc<Pipelines>>() {
                p.clone()
            } else {
                let p = Arc::new(Pipelines::new(&rs.device)?);
                renderer.callback_resources.insert(p.clone());
                p
            }
        };
        let texture = |label, format, usage| {
            rs.device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: wgpu::Extent3d { width: size[0], height: size[1], depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage,
                view_formats: &[],
            })
        };
        let input_usage = wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST;
        let rgb = texture("channel source", FLOAT, input_usage);
        let selection = texture("channel selection", wgpu::TextureFormat::R32Float, input_usage);
        // Upload bounded bands at native precision, once. No RGBA8 conversion of HDR samples.
        for y in (0..size[1]).step_by(32) {
            let rows = (size[1] - y).min(32);
            let r = photocraft_geom::Rect::from_xywh(0, y as i32, size[0], rows);
            let native = surf.try_read_region(r).ok()?;
            let channels = surf.channels();
            let mut rgba = Vec::new();
            rgba.try_reserve_exact(native.len().checked_div(channels)?.checked_mul(4)?).ok()?;
            for p in native.chunks_exact(channels) {
                rgba.extend_from_slice(p.get(..3)?);
                let alpha = p.get(3).copied().unwrap_or(1.0);
                if !(0.0..=1.0).contains(&alpha) {
                    return None;
                }
                rgba.push(alpha);
            }
            if rgba.iter().any(|v| !v.is_finite() || v.abs() > 65504.0) {
                return None;
            }
            write_band(rs, &rgb, size[0], y, rows, 4, &rgba);
            let values = match &doc.selection {
                Some(s) => s.try_read_region(r).ok()?,
                None => vec![1.0; (size[0] as usize).checked_mul(rows as usize)?],
            };
            if values.len() != (size[0] as usize).checked_mul(rows as usize)? || values.iter().any(|v| !(0.0..=1.0).contains(v)) {
                return None;
            }
            write_band(rs, &selection, size[0], y, rows, 1, &values);
        }
        let render_usage = wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::RENDER_ATTACHMENT;
        let moved = texture("transformed channel", FLOAT, render_usage);
        let output = texture("channel display", OUTPUT, render_usage | wgpu::TextureUsages::COPY_SRC);
        let moved_view = moved.create_view(&Default::default());
        let output_view = output.create_view(&Default::default());
        let uniform = rs.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("channel transform parameters"),
            size: UNIFORM_BYTES,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let inputs = rs.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("channel transform source"),
            layout: &pipes.inputs,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: uniform.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&rgb.create_view(&Default::default())) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(&selection.create_view(&Default::default())) },
            ],
        });
        let moved = rs.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("channel transform result"),
            layout: &pipes.moved,
            entries: &[wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&moved_view) }],
        });
        let id = rs.renderer.write().register_native_texture(&rs.device, &output_view, wgpu::FilterMode::Linear);
        let buffer = |label, usage| {
            rs.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size: 64,
                usage: usage | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            })
        };
        let steps = match doc.depth {
            SampleType::U8 => 255.0,
            SampleType::U16 => 65535.0,
            SampleType::F32 => 0.0,
        };
        let fill = fill.map(|v| if steps > 0.0 { (v.clamp(0.0, 1.0) * steps + 0.5).floor() / steps } else { v });
        Some(Self {
            rs: rs.clone(),
            pipes,
            uniform,
            inputs,
            moved,
            moved_view,
            output_view,
            _output: output,
            id,
            size,
            opacity: l.opacity.clamp(0.0, 1.0) * l.fill_opacity.clamp(0.0, 1.0),
            steps,
            fill,
            selected: doc.selection.is_some(),
            last: None,
            vertices: buffer("channel warp vertices", wgpu::BufferUsages::VERTEX),
            indices: buffer("channel warp triangles", wgpu::BufferUsages::INDEX),
        })
    }

    pub(crate) fn texture(&mut self, t: &TransformSession, view: &ChannelView, in_color: bool, encode: bool, factor: u32) -> Option<egui::TextureId> {
        let component = t.target.as_ref()?.get("color")?.as_u64()?;
        if component >= 3 {
            return None;
        }
        if self.last.as_ref().is_some_and(|(old, v, c, e)| old == t && v == view && *c == in_color && *e == encode) {
            return Some(self.id);
        }
        let k = f64::from(factor);
        let rect = t.rect.map(|v| v / k);
        let quad = t.quad.map(|p| p.map(|v| v / k));
        let h = photocraft_algo::transform::Homography::rect_to_quad(rect, quad)?;
        let denominators = [[rect[0], rect[1]], [rect[2], rect[1]], [rect[2], rect[3]], [rect[0], rect[3]]].map(|[x, y]| h.0[6] * x + h.0[7] * y + h.0[8]);
        if denominators.iter().any(|v| !v.is_finite() || v.abs() < 1e-12 || v.signum() != denominators[0].signum()) {
            return None;
        }
        let inv = h.inverse()?;
        // The CPU oracle prefilters strong reductions. Retain it until that filter is on GPU.
        if h.local_scale((rect[0] + rect[2]) * 0.5, (rect[1] + rect[3]) * 0.5) < 0.5 {
            return None;
        }
        let mut interp = match t.interpolation.as_str() {
            "nearest" => 0.0,
            "bilinear" => 1.0,
            _ => 2.0,
        };
        let m = h.0;
        let integer_translation = t.warp.is_none()
            && m.iter().all(|v| v.is_finite())
            && [m[0] - 1.0, m[1], m[3], m[4] - 1.0, m[6], m[7], m[8] - 1.0, m[2] - m[2].round(), m[5] - m[5].round()].iter().all(|v| v.abs() < 1e-9);
        // f32 inverse coordinates cannot reproduce the CPU's f64 nearest-neighbour ties under
        // fractional transforms. Preserve those pixel choices via the CPU oracle.
        if interp == 0.0 && !integer_translation {
            return None;
        }
        if integer_translation {
            interp = 0.0;
        }
        let mut data = Vec::with_capacity(28);
        for row in inv.0.as_chunks::<3>().0 {
            data.extend(row.iter().map(|v| *v as f32));
            data.push(0.0);
        }
        data.extend([self.size[0] as f32, self.size[1] as f32, component as f32, *self.fill.get(component as usize)?]);
        data.extend(rect.map(|v| v as f32));
        let visible = (0..3).fold(0u32, |v, c| v | (u32::from(view.color_visible(c)) << c));
        data.extend([visible as f32, f32::from(in_color), self.opacity, f32::from(encode)]);
        data.extend([interp, f32::from(self.selected), self.steps, f32::from(h.is_identity() && t.warp.as_ref().is_none_or(|w| w.is_identity()))]);
        if data.iter().any(|v| !v.is_finite()) {
            return None;
        }
        let geometry = match &t.warp {
            Some(w) => Some(warp_geometry(w, rect, k)?),
            None => None,
        };
        self.rs.queue.write_buffer(&self.uniform, 0, &bytes(&data));
        let mut encoder = self.rs.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("channel preview") });
        if let Some((vertices, indices)) = geometry {
            let vb = bytes(&vertices);
            let ib: Vec<u8> = indices.iter().flat_map(|v| v.to_le_bytes()).collect();
            if vb.len() > 8 * 1024 * 1024 || ib.len() > 8 * 1024 * 1024 {
                return None;
            }
            for (buffer, size, usage) in [(&mut self.vertices, vb.len(), wgpu::BufferUsages::VERTEX), (&mut self.indices, ib.len(), wgpu::BufferUsages::INDEX)]
            {
                if size as u64 > buffer.size() {
                    *buffer = self.rs.device.create_buffer(&wgpu::BufferDescriptor {
                        label: Some("channel warp geometry"),
                        size: (size as u64).next_power_of_two(),
                        usage: usage | wgpu::BufferUsages::COPY_DST,
                        mapped_at_creation: false,
                    });
                }
            }
            self.rs.queue.write_buffer(&self.vertices, 0, &vb);
            self.rs.queue.write_buffer(&self.indices, 0, &ib);
            {
                let mut pass = render_pass(&mut encoder, &self.moved_view);
                pass.set_pipeline(&self.pipes.warp);
                pass.set_bind_group(0, &self.inputs, &[]);
                pass.set_vertex_buffer(0, self.vertices.slice(..));
                pass.set_index_buffer(self.indices.slice(..), wgpu::IndexFormat::Uint32);
                pass.draw_indexed(0..u32::try_from(indices.len()).ok()?, 0, 0..1);
            }
            let mut pass = render_pass(&mut encoder, &self.output_view);
            pass.set_pipeline(&self.pipes.finish);
            pass.set_bind_group(0, &self.inputs, &[]);
            pass.set_bind_group(1, &self.moved, &[]);
            pass.draw(0..3, 0..1);
        } else {
            let mut pass = render_pass(&mut encoder, &self.output_view);
            pass.set_pipeline(&self.pipes.affine);
            pass.set_bind_group(0, &self.inputs, &[]);
            pass.draw(0..3, 0..1);
        }
        self.rs.queue.submit([encoder.finish()]);
        self.last = Some((t.clone(), view.clone(), in_color, encode));
        Some(self.id)
    }
}

impl Drop for Preview {
    fn drop(&mut self) {
        self.rs.renderer.write().free_texture(&self.id);
    }
}

#[cfg(test)]
impl Preview {
    pub(super) fn readback(&self) -> Vec<egui::Color32> {
        let row = (self.size[0] * 4).div_ceil(256) * 256;
        let buffer = self.rs.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("channel preview readback"),
            size: u64::from(row) * u64::from(self.size[1]),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = self.rs.device.create_command_encoder(&Default::default());
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo { texture: &self._output, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(row), rows_per_image: Some(self.size[1]) },
            },
            wgpu::Extent3d { width: self.size[0], height: self.size[1], depth_or_array_layers: 1 },
        );
        self.rs.queue.submit([encoder.finish()]);
        buffer.slice(..).map_async(wgpu::MapMode::Read, |result| result.unwrap());
        self.rs.device.poll(wgpu::PollType::Wait { submission_index: None, timeout: Some(std::time::Duration::from_secs(10)) }).unwrap();
        let data = buffer.slice(..).get_mapped_range().unwrap();
        let pixels = data
            .chunks(row as usize)
            .flat_map(|r| r[..self.size[0] as usize * 4].as_chunks::<4>().0.iter().map(|p| egui::Color32::from_rgba_premultiplied(p[0], p[1], p[2], p[3])))
            .collect();
        drop(data);
        buffer.unmap();
        pixels
    }
}

fn bytes(values: &[f32]) -> Vec<u8> {
    values.iter().flat_map(|v| v.to_le_bytes()).collect()
}

fn write_band(rs: &RenderState, texture: &wgpu::Texture, width: u32, y: u32, rows: u32, channels: u32, values: &[f32]) {
    rs.queue.write_texture(
        wgpu::TexelCopyTextureInfo { texture, mip_level: 0, origin: wgpu::Origin3d { x: 0, y, z: 0 }, aspect: wgpu::TextureAspect::All },
        &bytes(values),
        wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(width * channels * 4), rows_per_image: Some(rows) },
        wgpu::Extent3d { width, height: rows, depth_or_array_layers: 1 },
    );
}

fn warp_geometry(w: &photocraft_geom::warp::Warp, rect: [f64; 4], factor: f64) -> Option<(Vec<f32>, Vec<u32>)> {
    // Bound cloning, tessellation and index allocation before visiting input-driven knots.
    if !factor.is_finite() || factor <= 0.0 || w.mesh.as_ref().is_some_and(|m| m.us.len() > 65 || m.vs.len() > 65 || m.points.len() > 193 * 193) {
        return None;
    }
    let mesh = w.to_mesh(1, 1);
    if !mesh.is_valid() {
        return None;
    }
    let (us, vs) = crate::warp_preview::samples(&mesh, 1.5 / factor);
    let mut vertices = Vec::with_capacity(us.len().checked_mul(vs.len())?.checked_mul(4)?);
    for v in &vs {
        for u in &us {
            let src = [rect[0] + u * (rect[2] - rect[0]), rect[1] + v * (rect[3] - rect[1])];
            let dst = w.map(src[0] * factor, src[1] * factor);
            let dst = [dst.0 as f32 / factor as f32, dst.1 as f32 / factor as f32];
            if dst.iter().any(|v| !v.is_finite()) {
                return None;
            }
            vertices.extend([dst[0], dst[1], src[0] as f32, src[1] as f32]);
        }
    }
    // Strong reductions/folds need the CPU oracle's footprint filter. Also retain the
    // engine's row-major ordering for the GPU triangles, rather than the overlay's z-order.
    let mut indices = Vec::new();
    for j in 0..vs.len().saturating_sub(1) {
        for i in 0..us.len().saturating_sub(1) {
            let a = u32::try_from(j.checked_mul(us.len())?.checked_add(i)?).ok()?;
            let stride = u32::try_from(us.len()).ok()?;
            let point = |v: u32| -> Option<[f64; 2]> {
                let offset = usize::try_from(v).ok()?.checked_mul(4)?;
                Some([f64::from(*vertices.get(offset)?), f64::from(*vertices.get(offset.checked_add(1)?)?)])
            };
            let right_index = a.checked_add(1)?;
            let bottom_index = a.checked_add(stride)?;
            let diagonal_index = bottom_index.checked_add(1)?;
            let (origin, right, bottom, diagonal) = (point(a)?, point(right_index)?, point(bottom_index)?, point(diagonal_index)?);
            let dx = (us.get(i + 1)? - us.get(i)?) * (rect[2] - rect[0]);
            let dy = (vs.get(j + 1)? - vs.get(j)?) * (rect[3] - rect[1]);
            if dx <= 0.0 || dy <= 0.0 {
                return None;
            }
            // Check both triangles: a pulled fourth corner can fold just one of them.
            for (x0, x1, y0, y1) in [(origin, right, right, diagonal), (bottom, diagonal, origin, bottom)] {
                let x = [(x1[0] - x0[0]) / dx, (x1[1] - x0[1]) / dx];
                let y = [(y1[0] - y0[0]) / dy, (y1[1] - y0[1]) / dy];
                let determinant = x[0] * y[1] - x[1] * y[0];
                let trace = x[0] * x[0] + x[1] * x[1] + y[0] * y[0] + y[1] * y[1];
                let least = ((trace - (trace * trace - 4.0 * determinant * determinant).max(0.0).sqrt()) * 0.5).max(0.0).sqrt();
                if !least.is_finite() || determinant <= 0.0 || least < 0.5 {
                    return None;
                }
            }
            indices.extend([a, right_index, diagonal_index, a, diagonal_index, bottom_index]);
        }
    }
    Some((vertices, indices))
}

fn render_pass<'a>(encoder: &'a mut wgpu::CommandEncoder, view: &wgpu::TextureView) -> wgpu::RenderPass<'a> {
    encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some("channel preview"),
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view,
            depth_slice: None,
            resolve_target: None,
            ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT), store: wgpu::StoreOp::Store },
        })],
        depth_stencil_attachment: None,
        timestamp_writes: None,
        occlusion_query_set: None,
        multiview_mask: None,
    })
}

impl Pipelines {
    fn new(device: &wgpu::Device) -> Option<Self> {
        use std::future::Future;
        let validation = device.push_error_scope(wgpu::ErrorFilter::Validation);
        let internal = device.push_error_scope(wgpu::ErrorFilter::Internal);
        let result = Self::build(device);
        let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
        for pop in [internal.pop(), validation.pop()] {
            if let std::task::Poll::Ready(Some(e)) = std::pin::pin!(pop).poll(&mut cx) {
                log::warn!("channel preview uses CPU: {e}");
                return None;
            }
        }
        Some(result)
    }

    fn build(device: &wgpu::Device) -> Self {
        let tex = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: false },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let inputs = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("channel preview inputs"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: std::num::NonZeroU64::new(UNIFORM_BYTES),
                    },
                    count: None,
                },
                tex(1),
                tex(2),
            ],
        });
        let moved = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor { label: Some("channel preview moved"), entries: &[tex(0)] });
        let simple = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("channel preview"),
            bind_group_layouts: &[Some(&inputs)],
            immediate_size: 0,
        });
        let finish_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("channel preview finish"),
            bind_group_layouts: &[Some(&inputs), Some(&moved)],
            immediate_size: 0,
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("channel preview"),
            source: wgpu::ShaderSource::Wgsl(include_str!("channel_transform_gpu.wgsl").into()),
        });
        let pipe = |layout: &wgpu::PipelineLayout, vertex, fragment, format, mesh| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("channel preview"),
                layout: Some(layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some(vertex),
                    buffers: if mesh {
                        &[Some(wgpu::VertexBufferLayout {
                            array_stride: 16,
                            step_mode: wgpu::VertexStepMode::Vertex,
                            attributes: &[wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x4, offset: 0, shader_location: 0 }],
                        })]
                    } else {
                        &[]
                    },
                    compilation_options: Default::default(),
                },
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: Default::default(),
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some(fragment),
                    targets: &[Some(wgpu::ColorTargetState { format, blend: None, write_mask: wgpu::ColorWrites::ALL })],
                    compilation_options: Default::default(),
                }),
                multiview_mask: None,
                cache: None,
            })
        };
        let affine = pipe(&simple, "vs_full", "fs_affine", OUTPUT, false);
        let warp = pipe(&simple, "vs_mesh", "fs_warp", FLOAT, true);
        let finish = pipe(&finish_layout, "vs_full", "fs_finish", OUTPUT, false);
        Self { inputs, moved, affine, warp, finish }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use photocraft_color::Color;
    use photocraft_doc::{LayerMask, Size};
    use photocraft_geom::warp::{BezierMesh, Warp};

    #[test]
    fn unsupported_stacks_keep_the_cpu_oracle() {
        let doc = Document::with_background("eligibility", Size::new(4, 4), ColorMode::Rgb, SampleType::U8, Color::WHITE);
        let id = doc.layers[0].id;
        assert!(source(&doc, Some(id)).is_some());
        for case in 0..10 {
            let mut d = doc.clone();
            match case {
                0 => d.mode = ColorMode::Cmyk,
                1 => d.layers[0].mask = Some(LayerMask::reveal_all()),
                2 => d.layers[0].clipped = true,
                3 => d.layers[0].blend = BlendMode::Multiply,
                4 => d.layers[0].excluded_channels = 1,
                5 => d.layers[0].opacity = f32::NAN,
                6 => d.layers[0].visible = false,
                7 => d.layers.push(doc.layers[0].clone()),
                8 => d.layers[0].opacity = -1.0,
                9 => d.layers[0].fill_opacity = 2.0,
                _ => {}
            }
            assert!(source(&d, Some(id)).is_none(), "case={case}");
        }
        assert!(source(&doc, None).is_none());
    }

    #[test]
    fn invalid_or_strongly_reduced_warps_retain_the_cpu_filter() {
        let rect = [0.0, 0.0, 32.0, 24.0];
        let m = BezierMesh::identity(rect, 1, 1);
        assert!(warp_geometry(&Warp::custom(m.clone(), rect), rect, 1.0).is_some());
        for scale in [-1.0, 0.0, 0.2, f64::NAN, f64::INFINITY] {
            let w = Warp::custom(m.map_points(|p| [p[0] * scale, p[1]]), rect);
            assert!(warp_geometry(&w, rect, 1.0).is_none(), "scale={scale}");
        }
        let mut folded = m.clone();
        *folded.points.last_mut().unwrap() = [0.0, 0.0];
        assert!(warp_geometry(&Warp::custom(folded, rect), rect, 1.0).is_none());
        let w = Warp::custom(BezierMesh { us: vec![], vs: vec![], points: vec![] }, rect);
        assert!(warp_geometry(&w, rect, 1.0).is_none());
    }
}

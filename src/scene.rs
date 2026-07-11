//! wgpu scene renderer, driven from egui via a paint callback.

use crate::model::Model;
use bytemuck::{Pod, Zeroable};
use eframe::egui_wgpu::{self, wgpu};
use wgpu::util::DeviceExt;

const SHADER: &str = r#"
struct Uniforms {
    view_proj: mat4x4<f32>,
    eye: vec4<f32>,
    // x: selected surface id + 1 (0 = none)
    misc: vec4<u32>,
};

@group(0) @binding(0) var<uniform> u: Uniforms;

struct VsIn {
    @location(0) pos: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) color: vec4<f32>,
    @location(3) id: u32,
};

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) world: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) color: vec4<f32>,
    @location(3) @interpolate(flat) id: u32,
};

@vertex
fn vs_main(in: VsIn) -> VsOut {
    var out: VsOut;
    out.clip = u.view_proj * vec4<f32>(in.pos, 1.0);
    out.world = in.pos;
    out.normal = in.normal;
    out.color = in.color;
    out.id = in.id;
    return out;
}

@fragment
fn fs_mesh(in: VsOut) -> @location(0) vec4<f32> {
    let n = normalize(in.normal);
    let vdir = normalize(u.eye.xyz - in.world);
    let lum = 0.4 + 0.6 * abs(dot(n, vdir));
    var rgb = in.color.rgb * lum;
    if (u.misc.x != 0u && in.id + 1u == u.misc.x) {
        rgb = mix(rgb, vec3<f32>(1.0, 0.45, 0.05), 0.6);
    }
    return vec4<f32>(rgb, in.color.a);
}

@fragment
fn fs_line(in: VsOut) -> @location(0) vec4<f32> {
    var rgb = in.color.rgb;
    if (u.misc.x != 0u && in.id + 1u == u.misc.x) {
        rgb = vec3<f32>(1.0, 0.35, 0.0);
    }
    return vec4<f32>(rgb, in.color.a);
}
"#;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct Vertex {
    pub pos: [f32; 3],
    pub normal: [f32; 3],
    pub color: [f32; 4],
    pub id: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct Uniforms {
    pub view_proj: [[f32; 4]; 4],
    pub eye: [f32; 4],
    pub misc: [u32; 4],
}

/// Per-surface index ranges into the shared vertex buffer.
pub struct SurfaceIndices {
    pub tris: Vec<u32>,
    pub edges: Vec<u32>,
    pub transparent: bool,
}

pub struct SceneRenderer {
    mesh_pipeline: wgpu::RenderPipeline,
    mesh_pipeline_no_depth_write: wgpu::RenderPipeline,
    edge_pipeline: wgpu::RenderPipeline,
    overlay_pipeline: wgpu::RenderPipeline,
    bind_group: wgpu::BindGroup,
    uniform_buf: wgpu::Buffer,
    vertex_buf: wgpu::Buffer,
    edge_vertex_buf: wgpu::Buffer,
    opaque_idx: wgpu::Buffer,
    trans_idx: wgpu::Buffer,
    edge_idx: wgpu::Buffer,
    overlay_buf: wgpu::Buffer,
    opaque_count: u32,
    trans_count: u32,
    edge_count: u32,
    overlay_count: u32,
    pub per_surface: Vec<SurfaceIndices>,
}

/// Build the shared vertex buffers (mesh + dark edge copy) and per-surface
/// index lists from a model.
pub fn build_mesh(model: &Model) -> (Vec<Vertex>, Vec<Vertex>, Vec<SurfaceIndices>) {
    let mut verts = Vec::new();
    let mut edge_verts = Vec::new();
    let mut per_surface = Vec::new();
    const EDGE_COLOR: [f32; 4] = [0.08, 0.08, 0.1, 1.0];
    for (id, s) in model.surfaces.iter().enumerate() {
        let base = verts.len() as u32;
        let color = s.stype.color();
        for v in &s.verts {
            verts.push(Vertex {
                pos: (*v).into(),
                normal: s.normal.into(),
                color,
                id: id as u32,
            });
            edge_verts.push(Vertex {
                pos: (*v).into(),
                normal: s.normal.into(),
                color: EDGE_COLOR,
                id: id as u32,
            });
        }
        let tris = s.tris.iter().map(|i| base + i).collect();
        let n = s.verts.len() as u32;
        let edges = (0..n).flat_map(|i| [base + i, base + (i + 1) % n]).collect();
        per_surface.push(SurfaceIndices {
            tris,
            edges,
            transparent: s.stype.is_transparent(),
        });
    }
    (verts, edge_verts, per_surface)
}

const OVERLAY_CAP: usize = 4096;

impl SceneRenderer {
    pub fn new(
        device: &wgpu::Device,
        target_format: wgpu::TextureFormat,
        depth_format: wgpu::TextureFormat,
        samples: u32,
        verts: &[Vertex],
        edge_verts: &[Vertex],
        per_surface: Vec<SurfaceIndices>,
    ) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("scene shader"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });

        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("scene bgl"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("scene layout"),
            bind_group_layouts: &[Some(&bgl)],
            immediate_size: 0,
        });

        let vbuf_layout = wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<Vertex>() as u64,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3, 2 => Float32x4, 3 => Uint32],
        };

        let blend = wgpu::BlendState::ALPHA_BLENDING;
        let target = [Some(wgpu::ColorTargetState {
            format: target_format,
            blend: Some(blend),
            write_mask: wgpu::ColorWrites::ALL,
        })];

        let make_pipeline = |label: &str,
                             topology: wgpu::PrimitiveTopology,
                             fs: &str,
                             depth_write: bool,
                             depth_compare: wgpu::CompareFunction,
                             bias: wgpu::DepthBiasState| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vs_main"),
                    compilation_options: Default::default(),
                    buffers: &[vbuf_layout.clone()],
                },
                primitive: wgpu::PrimitiveState {
                    topology,
                    cull_mode: None,
                    ..Default::default()
                },
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: depth_format,
                    depth_write_enabled: Some(depth_write),
                    depth_compare: Some(depth_compare),
                    stencil: Default::default(),
                    bias,
                }),
                multisample: wgpu::MultisampleState {
                    count: samples,
                    ..Default::default()
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some(fs),
                    compilation_options: Default::default(),
                    targets: &target,
                }),
                multiview_mask: None,
                cache: None,
            })
        };

        let push_back = wgpu::DepthBiasState {
            constant: 2,
            slope_scale: 2.0,
            clamp: 0.0,
        };
        let mesh_pipeline = make_pipeline(
            "mesh",
            wgpu::PrimitiveTopology::TriangleList,
            "fs_mesh",
            true,
            wgpu::CompareFunction::Less,
            push_back,
        );
        let mesh_pipeline_no_depth_write = make_pipeline(
            "mesh transparent",
            wgpu::PrimitiveTopology::TriangleList,
            "fs_mesh",
            false,
            wgpu::CompareFunction::Less,
            push_back,
        );
        let edge_pipeline = make_pipeline(
            "edges",
            wgpu::PrimitiveTopology::LineList,
            "fs_line",
            false,
            wgpu::CompareFunction::LessEqual,
            Default::default(),
        );
        let overlay_pipeline = make_pipeline(
            "overlay",
            wgpu::PrimitiveTopology::LineList,
            "fs_line",
            false,
            wgpu::CompareFunction::Always,
            Default::default(),
        );

        let uniform_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("uniforms"),
            size: std::mem::size_of::<Uniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("scene bg"),
            layout: &bgl,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform_buf.as_entire_binding(),
            }],
        });

        let vertex_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("scene verts"),
            contents: bytemuck::cast_slice(verts),
            usage: wgpu::BufferUsages::VERTEX,
        });
        let edge_vertex_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("edge verts"),
            contents: bytemuck::cast_slice(edge_verts),
            usage: wgpu::BufferUsages::VERTEX,
        });

        let total_tris: usize = per_surface.iter().map(|s| s.tris.len()).sum();
        let total_edges: usize = per_surface.iter().map(|s| s.edges.len()).sum();
        let idx_buf = |label: &str, len: usize| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size: (len.max(3) * 4) as u64,
                usage: wgpu::BufferUsages::INDEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            })
        };
        let opaque_idx = idx_buf("opaque indices", total_tris);
        let trans_idx = idx_buf("transparent indices", total_tris);
        let edge_idx = idx_buf("edge indices", total_edges);

        let overlay_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("overlay verts"),
            size: (OVERLAY_CAP * std::mem::size_of::<Vertex>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        Self {
            mesh_pipeline,
            mesh_pipeline_no_depth_write,
            edge_pipeline,
            overlay_pipeline,
            bind_group,
            uniform_buf,
            vertex_buf,
            edge_vertex_buf,
            opaque_idx,
            trans_idx,
            edge_idx,
            overlay_buf,
            opaque_count: 0,
            trans_count: 0,
            edge_count: 0,
            overlay_count: 0,
            per_surface: Vec::from(per_surface),
        }
    }

    /// Rebuild index buffers for the given per-surface visibility.
    pub fn set_visibility(&mut self, queue: &wgpu::Queue, visible: &[bool]) {
        let mut opaque = Vec::new();
        let mut trans = Vec::new();
        let mut edges = Vec::new();
        for (s, &vis) in self.per_surface.iter().zip(visible) {
            if !vis {
                continue;
            }
            if s.transparent {
                trans.extend_from_slice(&s.tris);
            } else {
                opaque.extend_from_slice(&s.tris);
            }
            edges.extend_from_slice(&s.edges);
        }
        if !opaque.is_empty() {
            queue.write_buffer(&self.opaque_idx, 0, bytemuck::cast_slice(&opaque));
        }
        if !trans.is_empty() {
            queue.write_buffer(&self.trans_idx, 0, bytemuck::cast_slice(&trans));
        }
        if !edges.is_empty() {
            queue.write_buffer(&self.edge_idx, 0, bytemuck::cast_slice(&edges));
        }
        self.opaque_count = opaque.len() as u32;
        self.trans_count = trans.len() as u32;
        self.edge_count = edges.len() as u32;
    }

    fn update(&mut self, queue: &wgpu::Queue, uniforms: &Uniforms, overlay: &[Vertex]) {
        queue.write_buffer(&self.uniform_buf, 0, bytemuck::bytes_of(uniforms));
        let n = overlay.len().min(OVERLAY_CAP);
        if n > 0 {
            queue.write_buffer(&self.overlay_buf, 0, bytemuck::cast_slice(&overlay[..n]));
        }
        self.overlay_count = n as u32;
    }

    fn paint(&self, pass: &mut wgpu::RenderPass<'static>) {
        pass.set_bind_group(0, &self.bind_group, &[]);
        pass.set_vertex_buffer(0, self.vertex_buf.slice(..));

        if self.opaque_count > 0 {
            pass.set_pipeline(&self.mesh_pipeline);
            pass.set_index_buffer(self.opaque_idx.slice(..), wgpu::IndexFormat::Uint32);
            pass.draw_indexed(0..self.opaque_count, 0, 0..1);
        }
        if self.edge_count > 0 {
            pass.set_pipeline(&self.edge_pipeline);
            pass.set_vertex_buffer(0, self.edge_vertex_buf.slice(..));
            pass.set_index_buffer(self.edge_idx.slice(..), wgpu::IndexFormat::Uint32);
            pass.draw_indexed(0..self.edge_count, 0, 0..1);
        }
        if self.trans_count > 0 {
            pass.set_pipeline(&self.mesh_pipeline_no_depth_write);
            pass.set_vertex_buffer(0, self.vertex_buf.slice(..));
            pass.set_index_buffer(self.trans_idx.slice(..), wgpu::IndexFormat::Uint32);
            pass.draw_indexed(0..self.trans_count, 0, 0..1);
        }
        if self.overlay_count > 0 {
            pass.set_pipeline(&self.overlay_pipeline);
            pass.set_vertex_buffer(0, self.overlay_buf.slice(..));
            pass.draw(0..self.overlay_count, 0..1);
        }
    }
}

/// Per-frame paint callback handed to egui.
pub struct ViewCallback {
    pub uniforms: Uniforms,
    pub overlay: Vec<Vertex>,
    /// When Some, per-surface visibility changed and index buffers are rebuilt.
    pub visibility: Option<Vec<bool>>,
}

impl egui_wgpu::CallbackTrait for ViewCallback {
    fn prepare(
        &self,
        _device: &wgpu::Device,
        queue: &wgpu::Queue,
        _screen_descriptor: &egui_wgpu::ScreenDescriptor,
        _egui_encoder: &mut wgpu::CommandEncoder,
        callback_resources: &mut egui_wgpu::CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        if let Some(scene) = callback_resources.get_mut::<SceneRenderer>() {
            if let Some(vis) = &self.visibility {
                scene.set_visibility(queue, vis);
            }
            scene.update(queue, &self.uniforms, &self.overlay);
        }
        Vec::new()
    }

    fn paint(
        &self,
        _info: eframe::egui::PaintCallbackInfo,
        render_pass: &mut wgpu::RenderPass<'static>,
        callback_resources: &egui_wgpu::CallbackResources,
    ) {
        if let Some(scene) = callback_resources.get::<SceneRenderer>() {
            scene.paint(render_pass);
        }
    }
}

//! Boundary-face mesh extraction from a material-ID voxel grid, plus a small hand-rolled
//! glow (OpenGL) renderer to display it via `egui::PaintCallback`. There is no 3D-viewport
//! widget in egui itself, so a `materials.vtk` viewer means drawing the cubes ourselves.

use crate::postproc::vtkio::VtkGrid;
use eframe::glow::{self, HasContext as _};
use std::collections::HashSet;

pub(crate) struct Mesh {
    /// Interleaved per vertex: position(3), normal(3), color(3).
    vertices: Vec<f32>,
    vertex_count: i32,
    pub(crate) bounding_radius: f32,
    pub(crate) legend: Vec<(f64, [f32; 3])>,
}

const CUBE_CORNERS: [[f32; 3]; 8] = [
    [0.0, 0.0, 0.0],
    [1.0, 0.0, 0.0],
    [1.0, 1.0, 0.0],
    [0.0, 1.0, 0.0],
    [0.0, 0.0, 1.0],
    [1.0, 0.0, 1.0],
    [1.0, 1.0, 1.0],
    [0.0, 1.0, 1.0],
];

/// (normal, quad corner indices into `CUBE_CORNERS`, cyclic), one per axis-aligned face,
/// ordered to match the `(dx, dy, dz)` neighbor-offset order used in `build_boundary_mesh`.
const FACES: [([f32; 3], [usize; 4]); 6] = [
    ([1.0, 0.0, 0.0], [1, 2, 6, 5]),
    ([-1.0, 0.0, 0.0], [0, 4, 7, 3]),
    ([0.0, 1.0, 0.0], [3, 7, 6, 2]),
    ([0.0, -1.0, 0.0], [0, 1, 5, 4]),
    ([0.0, 0.0, 1.0], [4, 5, 6, 7]),
    ([0.0, 0.0, -1.0], [0, 3, 2, 1]),
];
const NEIGHBOR_OFFSETS: [(i64, i64, i64); 6] =
    [(1, 0, 0), (-1, 0, 0), (0, 1, 0), (0, -1, 0), (0, 0, 1), (0, 0, -1)];

fn hsv_to_rgb(h: f32, s: f32, v: f32) -> [f32; 3] {
    let h = h.fract().rem_euclid(1.0) * 6.0;
    let i = h.floor() as i32;
    let f = h - h.floor();
    let p = v * (1.0 - s);
    let q = v * (1.0 - s * f);
    let t = v * (1.0 - s * (1.0 - f));
    match i.rem_euclid(6) {
        0 => [v, t, p],
        1 => [q, v, p],
        2 => [p, v, t],
        3 => [p, q, v],
        4 => [t, p, v],
        _ => [v, p, q],
    }
}

/// Golden-angle hue stepping gives visually distinct colors for however many distinct
/// material IDs the grid turns out to have, without needing to know the count up front.
fn material_color(ordinal: usize) -> [f32; 3] {
    hsv_to_rgb(ordinal as f32 * 0.618_034, 0.55, 0.85)
}

/// Builds a triangle mesh of only the *boundary* faces of `grid`: domain edges, and
/// interfaces where two adjacent voxels have different material IDs. Faces between two
/// same-material voxels are always occluded by neighboring geometry, so skipping them keeps
/// the vertex count proportional to interface surface area rather than voxel count.
pub(crate) fn build_boundary_mesh(grid: &VtkGrid) -> Mesh {
    let (nx, ny, nz) = (grid.nx, grid.ny, grid.nz);

    let mut seen = HashSet::new();
    let mut ids: Vec<f64> = Vec::new();
    for &v in &grid.data {
        if seen.insert(v.to_bits()) {
            ids.push(v);
        }
    }
    ids.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let legend: Vec<(f64, [f32; 3])> =
        ids.iter().enumerate().map(|(i, &id)| (id, material_color(i))).collect();
    let color_of = |value: f64| legend[ids.partition_point(|&id| id < value)].1;

    let cell = |ix: usize, iy: usize, iz: usize| grid.data[ix + nx * (iy + ny * iz)];
    let spacing = [grid.dx as f32, grid.dy as f32, grid.dz as f32];
    let origin = [
        -(nx as f32) * spacing[0] / 2.0,
        -(ny as f32) * spacing[1] / 2.0,
        -(nz as f32) * spacing[2] / 2.0,
    ];

    let mut vertices: Vec<f32> = Vec::new();
    for iz in 0..nz {
        for iy in 0..ny {
            for ix in 0..nx {
                let id = cell(ix, iy, iz);
                let color = color_of(id);

                for (face_i, &(dx, dy, dz)) in NEIGHBOR_OFFSETS.iter().enumerate() {
                    let (nix, niy, niz) = (ix as i64 + dx, iy as i64 + dy, iz as i64 + dz);
                    let out_of_bounds = nix < 0
                        || niy < 0
                        || niz < 0
                        || nix >= nx as i64
                        || niy >= ny as i64
                        || niz >= nz as i64;
                    let draw = out_of_bounds
                        || cell(nix as usize, niy as usize, niz as usize) != id;
                    if !draw {
                        continue;
                    }

                    let (normal, corners) = FACES[face_i];
                    let world = |c: usize| {
                        let local = CUBE_CORNERS[corners[c]];
                        [
                            origin[0] + (ix as f32 + local[0]) * spacing[0],
                            origin[1] + (iy as f32 + local[1]) * spacing[1],
                            origin[2] + (iz as f32 + local[2]) * spacing[2],
                        ]
                    };
                    let mut push = |p: [f32; 3]| {
                        vertices.extend_from_slice(&p);
                        vertices.extend_from_slice(&normal);
                        vertices.extend_from_slice(&color);
                    };
                    let (p0, p1, p2, p3) = (world(0), world(1), world(2), world(3));
                    push(p0);
                    push(p1);
                    push(p2);
                    push(p0);
                    push(p2);
                    push(p3);
                }
            }
        }
    }

    let vertex_count = (vertices.len() / 9) as i32;
    let bounding_radius = 0.5
        * ((nx as f32 * spacing[0]).powi(2)
            + (ny as f32 * spacing[1]).powi(2)
            + (nz as f32 * spacing[2]).powi(2))
        .sqrt();

    Mesh { vertices, vertex_count, bounding_radius: bounding_radius.max(1e-3), legend }
}

/// GL objects for one uploaded mesh. Handles (`Program`/`Buffer`/`VertexArray`) are cheap
/// `Copy` wrappers around GL integer names, so this is freely cloneable to move into a
/// `'static` `egui_glow::CallbackFn` closure without needing an `Arc<Mutex<_>>`.
#[derive(Clone)]
pub(crate) struct GlRenderer {
    program: glow::Program,
    vao: glow::VertexArray,
    vbo: glow::Buffer,
    vertex_count: i32,
    u_mvp: Option<glow::UniformLocation>,
    u_light_dir: Option<glow::UniformLocation>,
}

fn shader_sources(version: eframe::egui_glow::ShaderVersion) -> (String, String) {
    use eframe::egui_glow::ShaderVersion;
    let legacy = matches!(version, ShaderVersion::Gl120 | ShaderVersion::Es100);
    let header = version.version_declaration();
    let precision = if matches!(version, ShaderVersion::Es100) { "precision mediump float;\n" } else { "" };

    let vertex = if legacy {
        format!(
            "{header}attribute vec3 a_pos;\nattribute vec3 a_normal;\nattribute vec3 a_color;\n\
             uniform mat4 u_mvp;\nvarying vec3 v_normal;\nvarying vec3 v_color;\n\
             void main() {{ v_normal = a_normal; v_color = a_color; gl_Position = u_mvp * vec4(a_pos, 1.0); }}\n"
        )
    } else {
        format!(
            "{header}in vec3 a_pos;\nin vec3 a_normal;\nin vec3 a_color;\n\
             uniform mat4 u_mvp;\nout vec3 v_normal;\nout vec3 v_color;\n\
             void main() {{ v_normal = a_normal; v_color = a_color; gl_Position = u_mvp * vec4(a_pos, 1.0); }}\n"
        )
    };

    let fragment = if legacy {
        format!(
            "{header}{precision}varying vec3 v_normal;\nvarying vec3 v_color;\nuniform vec3 u_light_dir;\n\
             void main() {{ float diffuse = max(dot(normalize(v_normal), u_light_dir), 0.0); \
             vec3 lit = v_color * (0.35 + diffuse * 0.65); gl_FragColor = vec4(lit, 1.0); }}\n"
        )
    } else {
        format!(
            "{header}in vec3 v_normal;\nin vec3 v_color;\nuniform vec3 u_light_dir;\nout vec4 out_color;\n\
             void main() {{ float diffuse = max(dot(normalize(v_normal), u_light_dir), 0.0); \
             vec3 lit = v_color * (0.35 + diffuse * 0.65); out_color = vec4(lit, 1.0); }}\n"
        )
    };

    (vertex, fragment)
}

fn compile_shader(gl: &glow::Context, kind: u32, src: &str) -> Result<glow::Shader, String> {
    unsafe {
        let shader = gl.create_shader(kind)?;
        gl.shader_source(shader, src);
        gl.compile_shader(shader);
        if !gl.get_shader_compile_status(shader) {
            return Err(gl.get_shader_info_log(shader));
        }
        Ok(shader)
    }
}

fn f32_slice_to_u8(v: &[f32]) -> &[u8] {
    // Safe: u8 has no alignment requirement and the byte length is an exact multiple.
    unsafe { std::slice::from_raw_parts(v.as_ptr().cast::<u8>(), std::mem::size_of_val(v)) }
}

impl GlRenderer {
    pub(crate) fn new(gl: &glow::Context, mesh: &Mesh) -> Result<Self, String> {
        unsafe {
            let version = eframe::egui_glow::ShaderVersion::get(gl);
            let (vertex_src, fragment_src) = shader_sources(version);

            let program = gl.create_program()?;
            let vertex_shader = compile_shader(gl, glow::VERTEX_SHADER, &vertex_src)?;
            let fragment_shader = compile_shader(gl, glow::FRAGMENT_SHADER, &fragment_src)?;
            gl.attach_shader(program, vertex_shader);
            gl.attach_shader(program, fragment_shader);
            gl.link_program(program);
            if !gl.get_program_link_status(program) {
                return Err(gl.get_program_info_log(program));
            }
            gl.detach_shader(program, vertex_shader);
            gl.detach_shader(program, fragment_shader);
            gl.delete_shader(vertex_shader);
            gl.delete_shader(fragment_shader);

            let vao = gl.create_vertex_array()?;
            let vbo = gl.create_buffer()?;
            gl.bind_vertex_array(Some(vao));
            gl.bind_buffer(glow::ARRAY_BUFFER, Some(vbo));
            gl.buffer_data_u8_slice(glow::ARRAY_BUFFER, f32_slice_to_u8(&mesh.vertices), glow::STATIC_DRAW);

            let stride = 9 * size_of::<f32>() as i32;
            for (name, offset_floats) in [("a_pos", 0), ("a_normal", 3), ("a_color", 6)] {
                if let Some(loc) = gl.get_attrib_location(program, name) {
                    gl.enable_vertex_attrib_array(loc);
                    gl.vertex_attrib_pointer_f32(
                        loc,
                        3,
                        glow::FLOAT,
                        false,
                        stride,
                        offset_floats * size_of::<f32>() as i32,
                    );
                }
            }
            gl.bind_vertex_array(None);
            gl.bind_buffer(glow::ARRAY_BUFFER, None);

            Ok(Self {
                program,
                vao,
                vbo,
                vertex_count: mesh.vertex_count,
                u_mvp: gl.get_uniform_location(program, "u_mvp"),
                u_light_dir: gl.get_uniform_location(program, "u_light_dir"),
            })
        }
    }

    /// Draws the mesh with the given model-view-projection matrix (column-major) and a
    /// world-space light direction. Only clears the depth buffer (scoped to the current
    /// scissor rect by the caller), never the color buffer, so whatever egui already
    /// painted behind this widget (e.g. a panel background) shows through as the backdrop.
    pub(crate) fn paint(&self, gl: &glow::Context, mvp: [f32; 16], light_dir: [f32; 3]) {
        unsafe {
            gl.clear(glow::DEPTH_BUFFER_BIT);
            gl.enable(glow::DEPTH_TEST);
            gl.depth_func(glow::LEQUAL);
            gl.disable(glow::CULL_FACE);
            gl.disable(glow::BLEND);

            gl.use_program(Some(self.program));
            gl.uniform_matrix_4_f32_slice(self.u_mvp.as_ref(), false, &mvp);
            gl.uniform_3_f32(self.u_light_dir.as_ref(), light_dir[0], light_dir[1], light_dir[2]);

            gl.bind_vertex_array(Some(self.vao));
            gl.draw_arrays(glow::TRIANGLES, 0, self.vertex_count);
            gl.bind_vertex_array(None);
        }
    }

    pub(crate) fn destroy(&self, gl: &glow::Context) {
        unsafe {
            gl.delete_program(self.program);
            gl.delete_buffer(self.vbo);
            gl.delete_vertex_array(self.vao);
        }
    }
}

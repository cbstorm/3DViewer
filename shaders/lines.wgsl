// Procedural line geometry (no vertex buffers): bounding boxes and the ground grid.

struct Frame {
    view_proj: mat4x4<f32>,
    camera_pos: vec4<f32>,
    light_dir: vec4<f32>,
    light_color: vec4<f32>,
    sky_color: vec4<f32>,
    ground_color: vec4<f32>,
};

struct Push {
    model: mat4x4<f32>,
    color: vec4<f32>,
    // x: grid line count per axis (grid only)
    params: vec4<f32>,
};

@group(0) @binding(0) var<uniform> frame: Frame;
var<push_constant> pc: Push;

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) color: vec4<f32>,
};

// Unit-cube edges as pairs of corner indices; corner bits are (x, y, z).
const EDGES = array<u32, 24>(
    0u, 1u, 2u, 3u, 4u, 5u, 6u, 7u, // along x
    0u, 2u, 1u, 3u, 4u, 6u, 5u, 7u, // along y
    0u, 4u, 1u, 5u, 2u, 6u, 3u, 7u, // along z
);

@vertex
fn vs_bounds(@builtin(vertex_index) vi: u32) -> VsOut {
    let corner = EDGES[vi];
    let p = vec3<f32>(f32(corner & 1u), f32((corner >> 1u) & 1u), f32((corner >> 2u) & 1u));
    var out: VsOut;
    out.clip = frame.view_proj * pc.model * vec4<f32>(p, 1.0);
    out.color = pc.color;
    return out;
}

@vertex
fn vs_grid(@builtin(vertex_index) vi: u32) -> VsOut {
    // `n` cells per axis gives n + 1 lines per axis, spanning [-n/2, n/2].
    let n = u32(pc.params.x);
    let lines_per_axis = n + 1u;
    let line = vi / 2u;
    let end = f32(vi % 2u) - 0.5;
    let half_n = f32(n) * 0.5;

    var p: vec3<f32>;
    var color = pc.color;
    if line < lines_per_axis {
        let x = f32(line) - half_n;
        p = vec3<f32>(x, 0.0, end * f32(n));
        if line * 2u == n {
            color = vec4<f32>(0.25, 0.4, 0.95, 0.9); // Z axis
        }
    } else {
        let z = f32(line - lines_per_axis) - half_n;
        p = vec3<f32>(end * f32(n), 0.0, z);
        if (line - lines_per_axis) * 2u == n {
            color = vec4<f32>(0.95, 0.3, 0.3, 0.9); // X axis
        }
    }

    var out: VsOut;
    out.clip = frame.view_proj * pc.model * vec4<f32>(p, 1.0);
    out.color = color;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    return in.color;
}

// Mesh shading: a metallic-roughness PBR model with one directional light
// and a hemispherical ambient term. Also renders the normal-visualization and
// flat-color (wireframe) view modes.

struct Frame {
    view_proj: mat4x4<f32>,
    camera_pos: vec4<f32>,
    // xyz: direction towards the light (world space), w: intensity
    light_dir: vec4<f32>,
    light_color: vec4<f32>,
    sky_color: vec4<f32>,
    ground_color: vec4<f32>,
};

struct Push {
    model: mat4x4<f32>,
    base_color: vec4<f32>,
    // rgb: emissive factor
    emissive: vec4<f32>,
    // x: metallic, y: roughness, z: mode (0 lit, 1 normals, 2 flat color), w: alpha cutoff
    params: vec4<f32>,
};

@group(0) @binding(0) var<uniform> frame: Frame;
@group(1) @binding(0) var base_color_tex: texture_2d<f32>;
@group(1) @binding(1) var metal_rough_tex: texture_2d<f32>;
@group(1) @binding(2) var emissive_tex: texture_2d<f32>;
@group(1) @binding(3) var tex_sampler: sampler;
var<push_constant> pc: Push;

struct VsIn {
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
};

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) world_pos: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
};

@vertex
fn vs_main(v: VsIn) -> VsOut {
    let world = pc.model * vec4<f32>(v.position, 1.0);
    // The cofactor matrix is the inverse-transpose scaled by the determinant;
    // after normalization it transforms normals correctly under non-uniform scale.
    let c0 = pc.model[0].xyz;
    let c1 = pc.model[1].xyz;
    let c2 = pc.model[2].xyz;
    let normal_matrix = mat3x3<f32>(cross(c1, c2), cross(c2, c0), cross(c0, c1));

    var out: VsOut;
    out.clip = frame.view_proj * world;
    out.world_pos = world.xyz;
    out.normal = normal_matrix * v.normal;
    out.uv = v.uv;
    return out;
}

const PI: f32 = 3.14159265;

fn distribution_ggx(n_dot_h: f32, roughness: f32) -> f32 {
    let a = roughness * roughness;
    let a2 = a * a;
    let d = n_dot_h * n_dot_h * (a2 - 1.0) + 1.0;
    return a2 / (PI * d * d);
}

fn geometry_smith(n_dot_v: f32, n_dot_l: f32, roughness: f32) -> f32 {
    let r = roughness + 1.0;
    let k = r * r / 8.0;
    let gv = n_dot_v / (n_dot_v * (1.0 - k) + k);
    let gl = n_dot_l / (n_dot_l * (1.0 - k) + k);
    return gv * gl;
}

fn fresnel_schlick(cos_theta: f32, f0: vec3<f32>) -> vec3<f32> {
    return f0 + (1.0 - f0) * pow(1.0 - cos_theta, 5.0);
}

@fragment
fn fs_main(in: VsOut, @builtin(front_facing) front_facing: bool) -> @location(0) vec4<f32> {
    let mode = u32(pc.params.z);
    if mode == 2u {
        return pc.base_color;
    }

    // Two-sided lighting: many models have inconsistent winding.
    var n = normalize(in.normal);
    if !front_facing {
        n = -n;
    }
    if mode == 1u {
        return vec4<f32>(n * 0.5 + 0.5, 1.0);
    }

    let base = pc.base_color * textureSample(base_color_tex, tex_sampler, in.uv);
    if base.a < pc.params.w {
        discard;
    }
    let mr = textureSample(metal_rough_tex, tex_sampler, in.uv);
    let metallic = clamp(pc.params.x * mr.b, 0.0, 1.0);
    let roughness = clamp(pc.params.y * mr.g, 0.04, 1.0);
    let emissive = pc.emissive.rgb * textureSample(emissive_tex, tex_sampler, in.uv).rgb;

    let v = normalize(frame.camera_pos.xyz - in.world_pos);
    let l = normalize(frame.light_dir.xyz);
    let h = normalize(v + l);
    let n_dot_l = max(dot(n, l), 0.0);
    let n_dot_v = max(dot(n, v), 1e-4);
    let n_dot_h = max(dot(n, h), 0.0);

    let f0 = mix(vec3<f32>(0.04), base.rgb, metallic);
    let f = fresnel_schlick(max(dot(h, v), 0.0), f0);
    let specular = distribution_ggx(n_dot_h, roughness) * geometry_smith(n_dot_v, n_dot_l, roughness) * f
        / (4.0 * n_dot_v * max(n_dot_l, 1e-4));
    let kd = (1.0 - f) * (1.0 - metallic);
    let direct = (kd * base.rgb / PI + specular) * frame.light_color.rgb * frame.light_dir.w * n_dot_l;

    let hemi = mix(frame.ground_color.rgb, frame.sky_color.rgb, n.y * 0.5 + 0.5) * frame.sky_color.w;
    let f_amb = fresnel_schlick(n_dot_v, f0);
    let ambient = hemi * ((1.0 - f_amb) * (1.0 - metallic) * base.rgb + f_amb * (1.0 - roughness * 0.7));

    return vec4<f32>(direct + ambient + emissive, base.a);
}

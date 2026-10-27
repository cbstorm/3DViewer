//! Compiles the WGSL shaders in `shaders/` to SPIR-V at build time using naga,
//! so no external shader compiler (glslc, glslangValidator) is required.

use std::{env, fs, path::PathBuf};

const SHADERS: &[(&str, &[(&str, naga::ShaderStage)])] = &[
    (
        "mesh",
        &[
            ("vs_main", naga::ShaderStage::Vertex),
            ("fs_main", naga::ShaderStage::Fragment),
        ],
    ),
    (
        "lines",
        &[
            ("vs_bounds", naga::ShaderStage::Vertex),
            ("vs_grid", naga::ShaderStage::Vertex),
            ("fs_main", naga::ShaderStage::Fragment),
        ],
    ),
];

fn main() {
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());

    for (name, entry_points) in SHADERS {
        let path = format!("shaders/{name}.wgsl");
        println!("cargo:rerun-if-changed={path}");
        let source = fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading {path}: {e}"));

        let module = naga::front::wgsl::parse_str(&source)
            .unwrap_or_else(|e| panic!("{}", e.emit_to_string_with_path(&source, &path)));
        let info = naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::PUSH_CONSTANT,
        )
        .validate(&module)
        .unwrap_or_else(|e| panic!("{}", e.emit_to_string_with_path(&source, &path)));

        let options = naga::back::spv::Options {
            lang_version: (1, 0),
            // Vulkan coordinate conventions are handled on the CPU side
            // (negative viewport height), so no coordinate adjustment here.
            flags: naga::back::spv::WriterFlags::empty(),
            ..Default::default()
        };

        for (entry_point, stage) in *entry_points {
            let pipeline_options = naga::back::spv::PipelineOptions {
                shader_stage: *stage,
                entry_point: entry_point.to_string(),
            };
            let words =
                naga::back::spv::write_vec(&module, &info, &options, Some(&pipeline_options))
                    .unwrap_or_else(|e| panic!("{path}:{entry_point}: {e}"));
            let bytes: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
            fs::write(out_dir.join(format!("{name}.{entry_point}.spv")), bytes).unwrap();
        }
    }
}

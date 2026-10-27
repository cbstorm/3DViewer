# 3D Viewer

A small, fast desktop viewer for **glTF/GLB, OBJ and STL** models, written in Rust on
Vulkan (`ash`) with an `egui` interface. It follows the design in [requirements.md](requirements.md).

## Features

- Formats: glTF 2.0 / GLB (node hierarchy, PBR metallic-roughness materials, base color /
  metal-rough / emissive textures, alpha mask and blend), OBJ + MTL (textures, transparency),
  binary and ASCII STL (flat-shaded, auto Z-up).
- Rendering: reverse-Z depth with an infinite far plane, 4x MSAA, one camera-attached
  directional light plus hemispheric ambient, two-sided shading, mipmapped textures.
- View modes: shaded, shaded + wireframe, wireframe, normals. Ground grid, scene bounds,
  highlighted bounds for the selected node.
- Camera: orbit, pan, zoom, fit-to-model, reset.
- UI: scene panel (stats, up axis, per-node visibility), material inspector with live
  base color / metallic / roughness editing, lighting controls, status bar with FPS,
  CPU and GPU frame time (timestamp queries), draw calls, triangles and culled primitives.
- Loading runs on a worker thread; the UI stays responsive and load errors show in a dialog.
- Screenshots: *File → Save screenshot…* (3D viewport only) or `--screenshot` (whole window).

## Requirements

- Rust 1.88+ (`rustup`), plus a C toolchain for linking (`sudo apt install build-essential` on
  Debian/Ubuntu).
- A Vulkan 1.2+ driver with dynamic rendering (core in 1.3, or `VK_KHR_dynamic_rendering`).
- Optional: the Khronos validation layer (`vulkan-validationlayers` package or the LunarG SDK).
  It is enabled automatically in debug builds when installed, or with `VIEWER_VALIDATION=1`.

No shader compiler is needed: `build.rs` compiles the WGSL shaders in [shaders/](shaders/) to
SPIR-V with `naga`.

## Build and run

```sh
cargo run --release                       # starts with a built-in demo scene
cargo run --release -- path/to/model.glb  # open a model directly
cargo run --release -- model.stl --screenshot out.png   # render once, save, exit
cargo test
```

Set `RUST_LOG=debug` for more logging.

## Release packages

`make help` lists all targets. Packages are written to `dist/`.

| Command | Output | Build on |
| --- | --- | --- |
| `make linux` | `viewer3d-<ver>-linux-x86_64.tar.gz` | Linux |
| `make windows` | `viewer3d-<ver>-windows-x86_64.zip` | Windows (MSVC), or Linux/macOS with MinGW-w64 (`apt install mingw-w64` / `brew install mingw-w64`) |
| `make windows-docker` | same zip, cross-compiled in Docker | Linux/macOS with Docker, no MinGW needed |
| `make macos` | `3D Viewer.app` (universal arm64 + x86_64) and `viewer3d-<ver>-macos-universal.zip` | macOS (Xcode command line tools) |
| `make dist` | every package the current host can build | any |

`make check-targets` type-checks the code for all three platforms from any host. It doesn't
link, so it works without the cross toolchains.

Platform notes for packaging:

- **Linux**: the binary links against the build machine's glibc, so build on the oldest
  distribution you want to support. Users need a Vulkan driver (`libvulkan1` plus Mesa or
  vendor drivers).
- **Windows**: Vulkan comes with the GPU driver. Release builds have no console window;
  startup errors show in a message box.
- **macOS**: cross-compiling from Linux isn't supported because winit links Apple frameworks
  from the macOS SDK. `make macos` bundles `libMoltenVK.dylib` from `$VULKAN_SDK` or Homebrew
  (`brew install molten-vk`), or from `MOLTENVK=/path/to/libMoltenVK.dylib`. The app loads it from
  `Contents/Frameworks`. The bundle is only ad-hoc signed, so sign and notarize it with your
  Developer ID before distributing.

## Controls

| Input | Action |
| --- | --- |
| Left drag | Orbit |
| Right / middle drag, Shift + left drag | Pan |
| Scroll | Zoom |
| Double-click, `F` | Fit to model |
| `R` | Reset camera |
| `1` – `4` | Shaded / shaded + wireframe / wireframe / normals |
| `G` / `B` | Toggle grid / bounds |
| `Ctrl+O` | Open model |
| `Esc` | Clear selection |
| `F1` | Controls help |

Files can also be dropped onto the window. winit 0.30 does not support file drops on
Wayland, so use *File → Open* there.

## Architecture

```
src/
├── main.rs            CLI parsing, event loop
├── app.rs             window, input routing, background loading, frame pacing
├── ui.rs              egui panels and shortcuts
├── assets/            format-specific importers → shared Scene representation
│   ├── import.rs      dispatch, vertex dedup, texture cache
│   ├── gltf.rs  obj.rs  stl.rs
├── scene/
│   ├── mesh.rs        Vertex, Primitive, Mesh, Aabb, normal generation
│   ├── scene.rs       Material, Texture, Node, Scene, up axis
│   ├── camera.rs      orbit camera, frustum culling
│   └── demo.rs        built-in startup scene
└── renderer/
    ├── device.rs      instance, validation, GPU selection, dynamic rendering
    ├── swapchain.rs   swapchain, depth + MSAA targets
    ├── memory.rs      gpu-allocator buffer/image wrappers
    ├── upload.rs      staged one-shot uploads, mipmap generation
    ├── gpu_scene.rs   shared vertex/index buffers, textures, material sets
    ├── pipeline.rs    pipelines and push-constant layouts
    ├── descriptors.rs descriptor layouts, sampler
    ├── frame.rs       per-frame command buffers, sync, timestamps
    └── mod.rs         frame loop, draw list, culling, egui pass
```

Notes:

- Importers run off the render thread and produce a format-independent `Scene`. Materials
  belong to primitives; nodes reference meshes, so instanced meshes aren't duplicated.
- All geometry for a scene goes into one device-local vertex buffer and one index buffer,
  uploaded through a single staging buffer. Draws are frustum-culled per primitive, sorted
  by material, and blended primitives are drawn back to front after opaque ones.
- After upload, CPU-side vertex and index data is released; only counts and bounds remain.
- Two frames in flight, per-swapchain-image "render finished" semaphores, and swapchain
  recreation on resize or `VK_ERROR_OUT_OF_DATE_KHR`.
- The scene pass renders into a multisampled target that resolves into the swapchain image.
  egui then draws on top in a second dynamic-rendering pass.

## Platform notes

- **Linux**: tested on Wayland (GNOME) with the NVIDIA driver. X11 works through winit.
- **Windows**: should work with any Vulkan 1.2+ driver, but is untested.
- **macOS**: needs MoltenVK (Vulkan SDK, or bundle `libMoltenVK.dylib` with the app). The
  portability enumeration and portability subset extensions are enabled automatically, and
  dynamic rendering falls back to `VK_KHR_dynamic_rendering`. Untested; validate a packaged
  build on a clean machine.

## Not yet implemented

Normal maps (no tangents), skinning and animation, glTF texture transforms and secondary UV
sets, image-based lighting, picking in the viewport, and multi-draw indirect.

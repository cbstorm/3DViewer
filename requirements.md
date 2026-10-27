# Recommended stack: Rust + Vulkan + native desktop

For a performance-oriented 3D model viewer targeting Windows, Linux, and macOS, I'd use `ash` for rendering, `winit` for the window, and `egui` for the UI, with a small custom renderer rather than a full game engine.

One important platform trade-off: macOS doesn't provide native Vulkan. Vulkan runs there through MoltenVK, which translates Vulkan calls to Metal. If you require direct Vulkan control, this is viable, but it adds deployment and compatibility work.&#x20;

[image](https://www.google.com/s2/favicons?domain=https://github.com&sz=32)

GitHub+1

## 1. Concrete dependency choices

| Layer             | Choice                   | Purpose                                 |
| ----------------- | ------------------------ | --------------------------------------- |
| Language          | Rust stable              | Application and renderer                |
| Window/event loop | `winit`                  | Native desktop windows and input        |
| Graphics API      | `ash`                    | Explicit Vulkan control                 |
| macOS Vulkan      | MoltenVK                 | Vulkan compatibility over Metal         |
| UI                | `egui` + `egui-winit`    | Panels, menus, inspectors               |
| UI rendering      | `egui-ash-renderer`      | Draw UI in your Vulkan command buffer   |
| GPU allocation    | `gpu-allocator`          | GPU buffer and image memory             |
| Math              | `glam`                   | Camera, matrices, transforms            |
| glTF              | `gltf`                   | Meshes, materials, scene nodes          |
| OBJ               | `tobj`                   | Triangle meshes and material references |
| STL               | `stl_io`                 | Triangle mesh import                    |
| Images            | `image`                  | Texture decoding                        |
| Vertex layout     | `bytemuck`               | Safe byte casting for GPU uploads       |
| Async loading     | `rayon` or `std::thread` | CPU-side loading and processing         |
| Diagnostics       | `tracing` + `anyhow`     | Logging and error handling              |

`winit` supports the major desktop platforms, and `egui-ash-renderer` provides a dedicated Vulkan renderer for egui. Its documented version compatibility matters, so pin a mutually compatible dependency set rather than independently upgrading graphics crates.&#x20;

[image](https://www.google.com/s2/favicons?domain=https://rust-windowing.github.io&sz=32)

Rust+1

### Suggested initial dependencies

Start with a small `Cargo.toml` and add crates as each subsystem is implemented. These are version-family examples, not a tested lockfile:

```
[dependencies]
ash = "0.38"
ash-window = "0.13"
winit = "0.30"
raw-window-handle = "0.6"

egui = "0.36"
egui-winit = "0.36"
egui-ash-renderer = "0.13"

glam = "0.30"
gltf = "1"
tobj = "4"
stl_io = "0.8"
image = "0.25"
bytemuck = { version = "1", features = ["derive"] }

anyhow = "1"
tracing = "0.1"
tracing-subscriber = "0.3"
```

Check the current crate releases and platform features when you create the lockfile. In particular, `ash-window` and window-handle APIs must match the chosen `winit` version.&#x20;

[image](https://www.google.com/s2/favicons?domain=https://docs.rs&sz=32)

Docs.rs+1

## 2. Renderer architecture

Keep the renderer independent of model import and UI logic. That makes performance tuning easier and prevents application state from becoming tangled with Vulkan resources.

Desktop application

winit · egui · file-open workflow

Scene and asset system

Scene graph · model importer · camera · transforms

GPU upload

Staging buffers · mesh and texture cache

Renderer

Pipelines · descriptors · draw lists

Vulkan frame

Depth prepass (optional) · opaque pass · UI pass · present

A practical source layout:

```
src/
├── main.rs
├── app.rs
├── assets/
│   ├── import.rs
│   ├── obj.rs
│   ├── gltf.rs
│   └── stl.rs
├── scene/
│   ├── scene.rs
│   ├── mesh.rs
│   └── camera.rs
├── renderer/
│   ├── device.rs
│   ├── swapchain.rs
│   ├── memory.rs
│   ├── upload.rs
│   ├── pipeline.rs
│   ├── descriptors.rs
│   └── frame.rs
└── ui.rs
```

## 3. Performance decisions that matter most

For a viewer, the biggest gains usually come from avoiding unnecessary draw calls, minimizing CPU overhead, and keeping geometry and textures on the GPU.

Batch meshes and minimize draw calls

Store vertex and index data in large GPU buffers. Group compatible primitives by material and pipeline, then render using indexed draws. Consider multi-draw indirect when scene size and driver support justify it.

Keep expensive work off the render thread

Decode models, parse files, calculate missing normals, and generate bounds in background workers. The render thread should handle GPU work and frame submission.

Use staged GPU uploads

Upload through staging buffers, then copy into device-local buffers. Reuse upload allocations and avoid creating a separate GPU allocation for every tiny mesh.

Build for stable frame pacing

Start with two or three frames in flight, per-frame command buffers, and explicit synchronization. Recreate swapchain-dependent resources correctly when the window resizes.

Add culling before advanced rendering

Compute mesh bounds and frustum-cull invisible objects. Add level of detail for very large models if profiling shows it is needed. Use a depth prepass only when it improves measured performance.

### Initial rendering features

I would start with:

- Indexed triangle rendering with a depth buffer.
- One directional light and a basic physically based material for glTF.
- Orbit, pan, zoom, fit-to-model, and reset-camera controls.
- Solid, wireframe, and normal-visualization modes.
- Object bounds and triangle/vertex counters.
- A simple material and scene inspector.

Do not begin with ray tracing, bindless descriptors, a render graph, or a complicated ECS. Those features add complexity before you know where the actual bottlenecks are.

## 4. Model import strategy

Normalize every supported format into one internal representation instead of teaching the renderer about OBJ, glTF, and STL separately.

| Format     | Import behavior                                           | Important caveat                                              |
| ---------- | --------------------------------------------------------- | ------------------------------------------------------------- |
| glTF / GLB | Preserve nodes, transforms, materials, textures, indices  | Support PBR materials and embedded assets                     |
| OBJ        | Import meshes, UVs, normals, material references          | Resolve accompanying MTL and texture files                    |
| STL        | Import triangle positions and optionally supplied normals | Usually has no materials or UVs; generate normals when needed |

For example, define a shared mesh representation:

```
struct MeshData {
    vertices: Vec<Vertex>,
    indices: Vec<u32>,
    bounds: Aabb,
}

struct Vertex {
    position: [f32; 3],
    normal: [f32; 3],
    uv: [f32; 2],
}
```

Treat material data and scene nodes as separate structures. This avoids duplicating geometry for every node that references the same mesh.

For performance and correctness:

- Deduplicate vertices where appropriate.
- Generate normals for STL and OBJ files when absent.
- Compute bounds during import.
- Use 32-bit indices as a straightforward default; split exceptionally large meshes if needed.
- Keep CPU asset data available only as long as needed, but retain it if you need reloads, picking, or recovery after GPU resource recreation.

## 5. Cross-platform strategy

[Logo Windows 11 – Logos PNG](https://images.openai.com/static-rsc-4/Mvc1fjsLrccfuc6_ABkaeeSs6zofXt09RRkQ2E0IapMoUHz6wD_x1mzHIAp_zdgWipqzxp62BWVGZl0UHy464JkxzjuOUAHyXdtleKNyldLAS9MxKgoG5ZmqO0HE1JcMWzxn3LyhBhM0rFmGrvAr1n8XhVYNTyFnHdZMb2TzG28?purpose=inline)

Windows

Native Vulkan drivers are available on supported GPUs. Test on both NVIDIA and AMD hardware, and include integrated GPUs in your compatibility testing where possible.

[Classic Tux Linux Mascot PNG | TOPpng](https://images.openai.com/static-rsc-4/lq70PRLKsJY7nZlytWYzDuE5S3mmpd2IyyKLnfkbsnewG4q6Zu8skMqW9Q95BYj5OfZ7vwOgdFybRkBgYgwabfZh-Fiei2TdnGfKfT3alTPJlp4f1srQYtjVNX1ek1qwspbMl9fQOypT1t1zWlH9UbSq98S6IEivrZytJlP6sxA?purpose=inline)

Linux

Test both X11 and Wayland, as well as the Vulkan drivers available on your target distributions. Document runtime library requirements for packaging.

[Use the Finder on Mac - Apple Support](https://images.openai.com/static-rsc-4/SpvpG0SQcOIQt87agT-VVODQtnacuxtyMwgi6g2SgdPx4aj6rZEZttu0W7clojNfU_6EyGbNvEFnGU1R5y1wgYLdEOXayxhiHRFgZLucHFDRiebCxapACwfCiteuOEJHMyfd8q_R-_4ZiOgmEVGH0GmqHMtIlNNJUpmtvsOrjHc?purpose=inline)

macOS

Bundle or otherwise distribute MoltenVK appropriately and test on Apple Silicon. Check the Vulkan features supported through MoltenVK rather than assuming every Vulkan feature is available.

The most important release concern is macOS distribution: a working local Vulkan SDK does not automatically mean the packaged app will find its Vulkan loader and MoltenVK libraries. Validate the actual packaged application on a clean machine.

If you want less platform-specific graphics work, `wgpu` is the alternative I'd consider. It offers Vulkan on Windows and Linux and a Metal backend on macOS, with a common Rust API. That can be a better choice when cross-platform deployment matters more than direct Vulkan access.&#x20;

[image](https://www.google.com/s2/favicons?domain=https://wgpu.rs&sz=32)

Rust+2

## 6. Implementation roadmap

A reasonable sequence for a solo developer is:

Phase 1

Foundation

Window and Vulkan bootstrap

Create the window, select a physical device, build the swapchain, clear the screen, and render a triangle. Verify all three operating systems early.

Phase 2

Renderer

Camera and indexed meshes

Implement depth testing, vertex/index buffers, staging uploads, an orbit camera, and a test cube. Add validation layers and debug labels.

Phase 3

Assets

Import all three formats

Normalize OBJ, glTF/GLB, and STL into shared mesh, material, and scene structures. Add asynchronous loading and error reporting.

Phase 4

Product UI

Add egui and viewer controls

Integrate the Vulkan UI renderer, file selection, material inspector, view modes, and scene statistics.

Phase 5

Optimization

Profile real workloads

Measure CPU frame time, GPU frame time, draw-call count, memory use, and model load latency. Add batching, culling, and more advanced rendering techniques only where measurements justify them.

## Final recommendation

Use `ash + winit + egui-ash-renderer + glam + gltf + tobj + stl_io + gpu-allocator` for the first implementation. Keep your renderer small, your asset pipeline format-independent, and your macOS build working from the beginning.

Your main architectural decision is whether the renderer is strictly Vulkan-specific or whether it should eventually support other graphics backends. If the app is explicitly a Vulkan learning/performance project, choose `ash`. If you want to spend more time on viewer functionality and less on backend differences, choose `wgpu`.

//! Release builds compile the compositor's shaders with D3D12's FXC optimizing. Test builds set
//! wgpu's debug flag, which skips FXC's optimizer, so a shader that FXC refuses only when
//! optimizing passed every test while the app fell back to the slow CPU compositor (user report,
//! 2026-10-24: "not all control paths return a value" in `blend_channel`, error X3507). This
//! builds the compositor the way the release app does on Windows.

use photocraft_gpu::Compositor;

#[test]
fn the_compositor_builds_with_fxc_optimizing_as_release_builds_do() {
    let _lock = super::GPU_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut desc = wgpu::InstanceDescriptor::new_without_display_handle();
    desc.backends = wgpu::Backends::DX12;
    // No DEBUG flag: FXC optimizes, as in a release build.
    desc.flags = wgpu::InstanceFlags::empty();
    desc.backend_options.dx12.shader_compiler = wgpu::Dx12Compiler::Fxc;
    let instance = wgpu::Instance::new(desc);
    let Ok(adapter) = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default())) else {
        eprintln!("skipping: no D3D12 adapter");
        return;
    };
    let Ok((device, _queue)) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())) else {
        eprintln!("skipping: no D3D12 device");
        return;
    };
    let format = Compositor::preferred_acc_format(&adapter);
    if let Err(e) = Compositor::try_new_with_format(&device, format) {
        panic!("{}: the compositor's shaders don't build with FXC optimizing: {e}", adapter.get_info().name);
    }
}

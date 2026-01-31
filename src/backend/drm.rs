use smithay::{
    backend::{
        drm::{DrmDevice, DrmDeviceFd, compositor::DrmCompositor},
        allocator::{gbm::{GbmDevice, GbmAllocator}, Fourcc},
        drm::exporter::gbm::GbmFramebufferExporter,
        egl::{EGLDisplay, EGLContext},
        renderer::{glow::GlowRenderer, ImportDma},
    },
    utils::{DeviceFd, Transform, Size, Scale},
    output::{Output, PhysicalProperties, Subpixel, Mode as OutputMode, OutputModeSource},
};
use smithay::reexports::drm::control::Device as _;
use tracing::info;
use std::{fs::OpenOptions, path::Path, os::unix::io::OwnedFd};
use anyhow::{Result, anyhow};

pub fn init_drm_graphics(device_path: &Path) -> Result<(
    GbmDevice<DrmDeviceFd>,
    EGLDisplay,
    GlowRenderer,
    Output,
    DrmCompositor<GbmAllocator<DrmDeviceFd>, GbmFramebufferExporter<DrmDeviceFd>, (), DrmDeviceFd>,
    DrmDevice,
    smithay::backend::drm::DrmDeviceNotifier,
)> {
    info!("Attempting to initialize DRM on: {:?}", device_path);

    let file = OpenOptions::new().read(true).write(true).append(true).open(device_path)
        .map_err(|e| anyhow!("Failed to open device {:?}: {}", device_path, e))?;
    
    let fd = DrmDeviceFd::new(DeviceFd::from(OwnedFd::from(file)));
    let (mut drm_device, notifier) = DrmDevice::new(fd.clone(), false)
        .map_err(|e| anyhow!("Failed to create DrmDevice: {}", e))?;
    
    let gbm = GbmDevice::new(fd)
        .map_err(|e| anyhow!("Gbm init failed: {}", e))?;
    
    let egl_display = unsafe { EGLDisplay::new(gbm.clone()) }
        .map_err(|e| anyhow!("EGL Display failed: {}", e))?;
    
    let egl_context = EGLContext::new(&egl_display)
        .map_err(|e| anyhow!("EGL Context failed: {}", e))?;
    
    let renderer = unsafe { GlowRenderer::new(egl_context) }
        .map_err(|e| anyhow!("Glow init failed: {}", e))?;

    let res_handles = drm_device.resource_handles()
        .map_err(|e| anyhow!("DRM handles failed: {}", e))?;
    
    let connector = res_handles.connectors().iter().find_map(|conn| {
        let info = drm_device.get_connector(*conn, false).ok()?;
        if info.state() == smithay::reexports::drm::control::connector::State::Connected {
            Some(*conn)
        } else {
            None
        }
    }).ok_or_else(|| anyhow!("No connected connector found on {:?}", device_path))?;

    let conn_info = drm_device.get_connector(connector, false)
        .map_err(|e| anyhow!("Failed to get connector info: {}", e))?;
    
    let mode = conn_info.modes().get(0)
        .ok_or_else(|| anyhow!("No modes found for connector {:?}", connector))?;
    
    let crtc = conn_info.encoders().iter().find_map(|&enc| {
        let info = drm_device.get_encoder(enc).ok()?;
        res_handles.filter_crtcs(info.possible_crtcs()).iter().next().copied()
    }).ok_or_else(|| anyhow!("No compatible CRTC found for connector {:?}", connector))?;

    let surface = drm_device.create_surface(crtc, *mode, &[connector])
        .map_err(|e| anyhow!("Failed to create DRM surface: {}", e))?;
    
    let allocator = GbmAllocator::new(gbm.clone(), smithay::backend::allocator::gbm::GbmBufferFlags::RENDERING | smithay::backend::allocator::gbm::GbmBufferFlags::SCANOUT);
    let exporter = GbmFramebufferExporter::new(gbm.clone(), None);

    let (w, h) = mode.size();
    let size = Size::from((w as i32, h as i32));
    
    let output_mode_source = OutputModeSource::Static {
        size,
        scale: Scale::from(1.0),
        transform: Transform::Normal,
    };

    let compositor = DrmCompositor::new(
        output_mode_source, surface, None, allocator, exporter,
        vec![Fourcc::Xrgb8888, Fourcc::Argb8888], renderer.dmabuf_formats(),
        Size::from((64, 64)), Some(gbm.clone()),
    ).map_err(|e| anyhow!("Compositor creation failed: {}", e))?;

    let output = Output::new(
        "Display-1".to_string(),
        PhysicalProperties {
            size: (500, 300).into(),
            subpixel: Subpixel::Unknown,
            make: "Flora".to_string(),
            model: "DRM Display".to_string(),
        },
    );
    
    let smithay_mode = OutputMode { size, refresh: (mode.vrefresh() * 1000) as i32 };
    output.change_current_state(
        Some(smithay_mode),
        Some(Transform::Normal),
        None,
        Some((0, 0).into()),
    );
    output.set_preferred(smithay_mode);

    Ok((gbm, egl_display, renderer, output, compositor, drm_device, notifier))
}

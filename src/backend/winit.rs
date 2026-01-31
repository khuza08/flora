use smithay::{
    backend::{
        winit::{self, WinitEventLoop, WinitGraphicsBackend},
        renderer::glow::GlowRenderer,
    },
    output::{Output, PhysicalProperties, Subpixel, Mode as OutputMode},
    utils::Transform,
};
use tracing::info;
use anyhow::{Result, anyhow};

pub fn init_winit_graphics() -> Result<(
    WinitGraphicsBackend<GlowRenderer>,
    WinitEventLoop,
    Output,
)> {
    info!("Initializing Winit backend (Nested Mode)...");

    let (backend, event_loop) = winit::init::<GlowRenderer>()
        .map_err(|e| anyhow!("Winit init failed: {}", e))?;

    let size = backend.window_size();
    let mode = OutputMode {
        size,
        refresh: 60_000, // 60Hz
    };

    let output = Output::new(
        "Winit-1".to_string(),
        PhysicalProperties {
            size: (0, 0).into(), // Unknown physical size
            subpixel: Subpixel::Unknown,
            make: "Winit".to_string(),
            model: "Nested Display".to_string(),
        },
    );

    output.change_current_state(
        Some(mode),
        Some(Transform::Normal),
        None,
        Some((0, 0).into()),
    );
    output.set_preferred(mode);

    Ok((backend, event_loop, output))
}

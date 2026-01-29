use smithay::reexports::wayland_server::Display;
use tracing::info;

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().init();
    info!("Flora: Preliminary Build Success");

    let mut _display: Display<()> = Display::new()?;
    
    info!("Wayland display created. Flora is ready for development.");
    Ok(())
}

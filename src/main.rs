use smithay::{
    reexports::{
        calloop::EventLoop,
        wayland_server::Display,
    },
    wayland::socket::ListeningSocketSource,
};
use tracing::info;
use std::time::Duration;

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().init();
    info!("Flora: Memulai Compositor...");

    // 1. Siapkan Event Loop
    let mut event_loop = EventLoop::try_new()?;
    let handle = event_loop.handle();
    
    // 2. Siapkan Wayland Display
    let display: Display<()> = Display::new()?;
    
    // 3. Siapkan Socket Wayland
    let source = ListeningSocketSource::new_auto()?;
    let socket_name = source.socket_name().to_os_string();
    info!("Flora aktif! Mendengarkan di: {:?}", socket_name);

    // Masukkan source socket ke event loop
    handle.insert_source(source, |_client_stream, _, _state| {
        // Di sini kita akan handle pendaftaran client nanti
        info!("Client baru mencoba terhubung!");
    })?;

    // 4. Jalankan Loop Selamanya
    loop {
        let _ = display.dispatch_clients(&mut ());
        event_loop.dispatch(Duration::from_millis(16), &mut ())?;
    }
}

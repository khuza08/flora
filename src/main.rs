use smithay::{
    backend::udev::{UdevBackend, UdevEvent},
    reexports::{
        calloop::{EventLoop, Interest, Mode, PostAction},
        wayland_server::{Display, DisplayHandle, Client, backend::ClientData},
    },
    wayland::{
        compositor::{CompositorState, CompositorHandler, CompositorClientState},
        socket::ListeningSocketSource,
    },
};
use tracing::info;
use std::{time::Duration, sync::Arc, path::PathBuf};

pub struct FloraState {
    pub display_handle: DisplayHandle,
    pub compositor_state: CompositorState,
    pub should_stop: bool,
    pub drm_devices: Vec<PathBuf>,
}

pub struct FloraClientData {
    pub compositor_state: CompositorClientState,
}

impl ClientData for FloraClientData {}

impl FloraState {
    pub fn new(dh: &DisplayHandle) -> Self {
        let compositor_state = CompositorState::new::<Self>(dh);

        Self {
            display_handle: dh.clone(),
            compositor_state,
            should_stop: false,
            drm_devices: Vec::new(),
        }
    }
}

// Implementasi trait dasar Smithay
impl CompositorHandler for FloraState {
    fn compositor_state(&mut self) -> &mut CompositorState {
        &mut self.compositor_state
    }

    fn client_compositor_state<'a>(&self, client: &'a Client) -> &'a CompositorClientState {
        &client.get_data::<FloraClientData>().unwrap().compositor_state
    }

    // Callback saat client membuat surface baru
    fn commit(&mut self, _surface: &smithay::reexports::wayland_server::protocol::wl_surface::WlSurface) {}
}

// Makro delegate untuk menghubungkan FloraState dengan Smithay
smithay::delegate_compositor!(FloraState);

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().init();
    info!("Flora: Memulai Compositor macOS-like...");

    // 1. Siapkan Event Loop
    let mut event_loop: EventLoop<FloraState> = EventLoop::try_new()?;
    let handle = event_loop.handle();
    
    // 2. Siapkan Wayland Display
    let display = Display::new()?;
    let dh = display.handle();
    
    // 3. Inisialisasi State
    let mut state = FloraState::new(&dh);
    
    // 4. Siapkan Socket Wayland
    let source = ListeningSocketSource::new_auto()?;
    let socket_name = source.socket_name().to_os_string();
    info!("Flora aktif! Socket Name: {:?}", socket_name);

    handle.insert_source(source, |client_stream, _, state| {
        let client_data = FloraClientData {
            compositor_state: CompositorClientState::default(),
        };
        let _ = state.display_handle.insert_client(client_stream, Arc::new(client_data));
        info!("Client baru terhubung!");
    }).map_err(|_e| anyhow::anyhow!("Gagal memasukkan source socket"))?;

    // 5. Inisialisasi Udev Backend (untuk mendeteksi display di VM)
    // Gunakan "seat0" karena itu adalah standar di Arch Linux
    let udev = UdevBackend::new("seat0")?;
    handle.insert_source(udev, |event, _, state| {
        match event {
            UdevEvent::Added { device_id: _, path } => {
                info!("Perangkat baru terdeteksi: {:?}", path);
                // Simpan jika ini adalah perangkat DRM (kartu grafis)
                if path.to_string_lossy().contains("card") || path.to_string_lossy().contains("render") {
                    state.drm_devices.push(path);
                }
            },
            UdevEvent::Changed { device_id: _ } => info!("Perangkat berubah"),
            UdevEvent::Removed { device_id: _ } => info!("Perangkat dihapus"),
        }
    }).map_err(|_e| anyhow::anyhow!("Gagal memasukkan source udev"))?;

    // Masukkan Wayland Display ke event loop
    handle.insert_source(
        smithay::reexports::calloop::generic::Generic::new(display, Interest::READ, Mode::Level),
        |_, display, state| {
            unsafe {
                display.get_mut().dispatch_clients(state).map(|_| PostAction::Continue)
            }
        },
    ).map_err(|_e| anyhow::anyhow!("Gagal memasukkan source display"))?;

    // 6. Jalankan Loop
    info!("Flora Loop dimulai. Menunggu hardware grafis...");
    while !state.should_stop {
        // Jika ada perangkat DRM yang ditemukan tapi belum diinisialisasi, kita bisa inisialisasi di sini
        // (Untuk saat ini kita hanya log, implementasi rendering penuh akan menyusul di langkah berikutnya)
        if !state.drm_devices.is_empty() {
             let device = state.drm_devices.pop().unwrap();
             info!("Mencoba inisialisasi DRM pada: {:?}", device);
             // Di sini nantinya kita akan panggil DrmBackend::new
        }

        event_loop.dispatch(Duration::from_millis(16), &mut state)?;
    }

    Ok(())
}

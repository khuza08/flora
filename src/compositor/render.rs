use smithay::{
    backend::renderer::{
        glow::GlowRenderer,
        element::{Kind, surface::{render_elements_from_surface_tree, WaylandSurfaceRenderElement}, solid::SolidColorRenderElement, texture::TextureRenderElement},
    },
    utils::{Point, Physical, Rectangle, Size, Scale, IsAlive},
    wayland::compositor::{SurfaceAttributes, with_states},
    reexports::wayland_server::{Display, protocol::{wl_surface::WlSurface, wl_callback::WlCallback}},
};
use smithay::backend::renderer::gles::GlesTexture;
use smithay_egui::EguiState;
use std::{rc::Rc, cell::RefCell};
use anyhow::Result;
use tracing::{info, error};

use crate::compositor::state::{FloraState, BackendData};
use crate::compositor::window::TITLE_BAR_HEIGHT;

smithay::backend::renderer::element::render_elements! {
    pub CustomRenderElement<=GlowRenderer>;
    Surface=WaylandSurfaceRenderElement<GlowRenderer>,
    Solid=SolidColorRenderElement,
    Egui=TextureRenderElement<GlesTexture>,
}

pub fn render_frame(state: &mut FloraState, display: &Rc<RefCell<Display<FloraState>>>) -> Result<()> {
    use tracing::debug;
    debug!("render_frame called, windows: {}", state.windows.len());
    
    let output_size = state.output.as_ref()
        .and_then(|o| o.current_mode())
        .map(|m| m.size)
        .ok_or_else(|| anyhow::anyhow!("No output mode"))?;
        
    let scale = get_output_scale(state);
    let color = [0.1, 0.1, 0.1, 1.0];

    // Collect window data for egui
    let window_data: Vec<_> = state.windows.iter().enumerate().map(|(idx, w)| {
        let surface_size = w.toplevel.current_state().size.unwrap_or((800, 600).into());
        let surface_size_physical = surface_size.to_physical(Scale::from(scale as i32));
        let is_focused = state.seat.get_keyboard()
            .and_then(|k| k.current_focus())
            .map(|s| s == *w.toplevel.wl_surface())
            .unwrap_or(false);
        (idx, w.location, surface_size_physical, is_focused, w.bar_id.clone())
    }).collect();

    match &mut state.backend_data {
        BackendData::Drm { compositor, .. } => {
            let mut renderer = state.renderer.as_mut().ok_or_else(|| anyhow::anyhow!("No renderer"))?;
            
            let (elements, pending_close) = gather_elements(
                &mut state.egui_state, 
                &state.windows, 
                &window_data[..],
                &mut renderer, 
                output_size, 
                scale,
                state.pointer_location,
                state.cursor_surface.as_ref(),
                state.cursor_hotspot,
            )?;

            if let Some(idx) = pending_close {
                state.windows[idx].toplevel.send_close();
            }

            if let Err(e) = compositor.render_frame::<GlowRenderer, CustomRenderElement>(&mut renderer, &elements, color, smithay::backend::drm::compositor::FrameFlags::empty()) {
                if format!("{:?}", e) != "EmptyFrame" {
                    error!("Rendering: render_frame failed: {:?}", e);
                }
            }
            if let Err(e) = compositor.commit_frame() {
                if format!("{:?}", e) != "EmptyFrame" {
                    error!("Rendering: commit_frame failed: {:?}", e);
                }
            }
        }
        #[cfg(feature = "winit")]
        BackendData::Winit { backend, damage_tracker } => {
            // Smart damage tracking: detect cursor movement
            let cursor_moved = state.pointer_location != state.last_pointer_location;
            
            // Use buffer_age = 0 when cursor moves (forces redraw of cursor area)
            // Use real buffer_age when idle (allows damage tracker to skip rendering)
            let buffer_age = if cursor_moved {
                state.last_pointer_location = state.pointer_location;
                0  // Force redraw when cursor moves
            } else {
                backend.buffer_age().unwrap_or(0)
            };
            
            let res = (|| -> Result<(Vec<CustomRenderElement>, Option<usize>)> {
                let (renderer, mut framebuffer) = backend.bind().map_err(|e| anyhow::anyhow!("Bind failed: {}", e))?;
                
                let (elements, pending_close) = gather_elements(
                    &mut state.egui_state, 
                    &state.windows, 
                    &window_data[..],
                    renderer, 
                    output_size, 
                    scale,
                    state.pointer_location,
                    state.cursor_surface.as_ref(),
                    state.cursor_hotspot,
                )?;

                if pending_close.is_none() {
                    // Damage tracker will optimize based on buffer_age
                    damage_tracker.render_output(
                        renderer,
                        &mut framebuffer,
                        buffer_age,
                        &elements,
                        color,
                    ).map_err(|e| anyhow::anyhow!("Damage tracker failed: {:?}", e))?;
                }
                
                Ok((elements, pending_close))
            })();

            match res {
                Ok((_elements, pending_close)) => {
                    if let Some(idx) = pending_close {
                        state.windows[idx].toplevel.send_close();
                    }
                    // Submit frame
                    if let Err(e) = backend.submit(None) {
                        error!("Winit submit failed: {:?}", e);
                    }
                }
                Err(err) => {
                    error!("Winit rendering failed: {:?}", err);
                }
            }
        }
        _ => {}
    }

    // Refresh display
    let _ = display.borrow_mut().flush_clients();
    
    // Send frame callbacks
    let time = state.start_time.elapsed().as_millis() as u32;
    for window in &state.windows {
        with_states(window.toplevel.wl_surface(), |states| {
            let mut attributes = states.cached_state.get::<SurfaceAttributes>();
            let current = attributes.current();
            for callback in current.frame_callbacks.drain(..) {
                let callback: WlCallback = callback;
                callback.done(time);
            }
        });
    }
    
    Ok(())
}

fn gather_elements(
    egui_state: &mut EguiState,
    windows: &[crate::compositor::Window],
    window_data: &[(usize, Point<i32, Physical>, Size<i32, Physical>, bool, smithay::backend::renderer::element::Id)],
    renderer: &mut GlowRenderer,
    output_size: Size<i32, Physical>,
    scale: f64,
    pointer_location: Point<f64, Physical>,
    cursor_surface: Option<&WlSurface>,
    cursor_hotspot: Point<i32, Physical>,
) -> Result<(Vec<CustomRenderElement>, Option<usize>)> {
    let mut elements = Vec::new();
    let mut pending_close = None;

    let egui_element = egui_state.render(
        |ctx| {
            egui::Area::new("flora_overlay".into())
                .fixed_pos(egui::pos2(0.0, 0.0))
                .show(ctx, |ui| {
                    for (idx, pos, size, is_focused, _) in window_data {
                        let logical_pos = pos.to_logical(Scale::from(scale as i32));
                        let logical_size = size.to_logical(Scale::from(scale as i32));
                        
                        let rect = egui::Rect::from_min_size(
                            egui::pos2(logical_pos.x as f32, logical_pos.y as f32),
                            egui::vec2(logical_size.w as f32, TITLE_BAR_HEIGHT as f32)
                        );

                        let color = if *is_focused {
                            egui::Color32::from_rgba_unmultiplied(40, 40, 40, 200)
                        } else {
                            egui::Color32::from_rgba_unmultiplied(30, 30, 30, 180)
                        };

                        ui.painter().rect_filled(rect, 5.0, color);

                        // Window title buttons
                        let btn_radius = 6.0;
                        let btn_padding = 10.0;
                        let btn_y = logical_pos.y as f32 + TITLE_BAR_HEIGHT as f32 / 2.0;
                        
                        let colors = [
                            egui::Color32::from_rgb(255, 95, 87),   // Close
                            egui::Color32::from_rgb(255, 189, 46),  // Minimize
                            egui::Color32::from_rgb(40, 201, 64),   // Maximize
                        ];

                        for (i, &base_color) in colors.iter().enumerate() {
                            let center = egui::pos2(logical_pos.x as f32 + btn_padding + (i as f32 * 20.0), btn_y);
                            let response = ui.interact(
                                egui::Rect::from_center_size(center, egui::vec2(btn_radius * 2.0, btn_radius * 2.0)),
                                egui::Id::new(format!("btn_{}_{}", idx, i)),
                                egui::Sense::click()
                            );

                            let final_color = if response.hovered() { base_color } else { base_color.gamma_multiply(0.8) };
                            ui.painter().circle_filled(center, btn_radius, final_color);

                            if response.clicked() && i == 0 {
                                info!("Flora: Closing window {}", idx);
                                pending_close = Some(*idx);
                            }
                        }
                    }
                });
        },
        renderer,
        Rectangle::new((0, 0).into(), (output_size.w, output_size.h).into()),
        scale,
        scale as f32,
    );

    match egui_element {
        Ok(egui_tex) => elements.push(CustomRenderElement::Egui(egui_tex)),
        Err(err) => error!("Failed to render egui overlay: {:?}", err),
    }

    for window in windows.iter().rev() {
        let surface_location = Point::from((window.location.x, window.location.y + TITLE_BAR_HEIGHT));
        elements.extend(render_elements_from_surface_tree::<GlowRenderer, CustomRenderElement>(
            renderer, 
            window.toplevel.wl_surface(), 
            surface_location, 
            1.0, 1.0, 
            Kind::Unspecified
        ));

        let surface_size = window.toplevel.current_state().size.unwrap_or((800, 600).into());
        let bar_rect = Rectangle::new(window.location, (surface_size.w, TITLE_BAR_HEIGHT).into());
        elements.push(CustomRenderElement::Solid(SolidColorRenderElement::new(
            window.bar_id.clone(),
            bar_rect,
            window.bar_commit_counter.clone(),
            [0.15, 0.15, 0.15, 1.0],
            Kind::Unspecified
        )));
    }
    
    // Cursor rendering
    if let Some(surface) = cursor_surface {
        if surface.alive() {
            // cursor_hotspot is already in Physical coordinates from Wayland protocol
            let render_pos = pointer_location - cursor_hotspot.to_f64();

            elements.extend(render_elements_from_surface_tree::<GlowRenderer, CustomRenderElement>(
                renderer,
                surface,
                render_pos.to_i32_round(),
                1.0, 1.0,
                Kind::Cursor,
            ));
        }
    }

    Ok((elements, pending_close))
}

pub fn get_output_scale(state: &FloraState) -> f64 {
    state.output.as_ref()
        .and_then(|o| o.current_scale().fractional_scale().into())
        .unwrap_or(1.0)
}

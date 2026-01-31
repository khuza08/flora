use smithay::{
    backend::renderer::{
        glow::GlowRenderer,
        element::{Kind, surface::{render_elements_from_surface_tree, WaylandSurfaceRenderElement}, solid::SolidColorRenderElement, texture::TextureRenderElement},
    },
    utils::{Point, Physical, Rectangle, Size},
    wayland::compositor::{with_surface_tree_downward, TraversalAction, SurfaceAttributes},
    reexports::wayland_server::Display,
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
    let scale = get_output_scale(state);
    
    let output_size = match &state.backend_data {
        BackendData::Drm { .. } => {
            state.output.as_ref()
                .and_then(|o| o.current_mode())
                .map(|m| m.size)
                .unwrap_or((1280, 800).into())
        }
        #[cfg(feature = "winit")]
        BackendData::Winit { backend, .. } => {
            backend.window_size()
        }
        BackendData::None => return Ok(()),
    };

    let color = [0.1, 0.1, 0.1, 1.0];

    let focused_surface = state.seat.get_keyboard().and_then(|kb| kb.current_focus());
    let window_data: Vec<(usize, Point<i32, Physical>, Size<i32, Physical>, bool, smithay::backend::renderer::element::Id)> = state.windows.iter().enumerate().map(|(idx, w)| {
        let surface_size = w.toplevel.current_state().size.unwrap_or((800, 600).into());
        let surface_size_physical = Size::from((surface_size.w as i32, surface_size.h as i32));
        let is_focused = focused_surface.as_ref().map(|f| f == w.toplevel.wl_surface()).unwrap_or(false);
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
                scale
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
            let buffer_age = backend.buffer_age().unwrap_or(0);
            
            let res = (|| -> Result<(Vec<CustomRenderElement>, Option<usize>)> {
                let (renderer, mut framebuffer) = backend.bind().map_err(|e| anyhow::anyhow!("Bind failed: {}", e))?;
                
                let (elements, pending_close) = gather_elements(
                    &mut state.egui_state, 
                    &state.windows, 
                    &window_data[..],
                    renderer, 
                    output_size, 
                    scale
                )?;

                if pending_close.is_none() {
                    damage_tracker.render_output(
                        renderer,
                        &mut framebuffer,
                        buffer_age,
                        &elements,
                        color,
                    ).map_err(|e| anyhow::anyhow!("Winit render failed: {}", e))?;
                }
                
                Ok((elements, pending_close))
            })();
            
            match res {
                Ok((_, pending_close_idx)) => {
                    if let Some(idx) = pending_close_idx {
                        state.windows[idx].toplevel.send_close();
                    }
                    if let Err(e) = backend.submit(None) {
                        error!("Winit Submit Error: {:?}", e);
                    }
                }
                Err(e) => {
                    error!("Winit Rendering Error: {:?}", e);
                }
            }
        }
        BackendData::None => {}
    }
    
    let time = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u32;
    for window in &state.windows {
        with_surface_tree_downward(window.toplevel.wl_surface(), (), |_, _, _| TraversalAction::DoChildren(()), |_, states, _| {
            let mut guard = states.cached_state.get::<SurfaceAttributes>();
            for callback in guard.current().frame_callbacks.drain(..) { 
                let c: smithay::reexports::wayland_server::protocol::wl_callback::WlCallback = callback;
                let _ = c.done(time); 
            }
        }, |_, _, _| true);
    }

    let _ = display.borrow_mut().flush_clients();
    state.needs_redraw = false;
    Ok(())
}

fn gather_elements(
    egui_state: &mut EguiState,
    windows: &[crate::compositor::Window],
    window_data: &[(usize, Point<i32, Physical>, Size<i32, Physical>, bool, smithay::backend::renderer::element::Id)],
    renderer: &mut GlowRenderer,
    output_size: Size<i32, Physical>,
    scale: f64,
) -> Result<(Vec<CustomRenderElement>, Option<usize>)> {
    let mut elements = Vec::new();
    let mut pending_close = None;

    let egui_element = egui_state.render(
        |ctx| {
            egui::Area::new(egui::Id::new("flora_ui"))
                .fixed_pos(egui::pos2(0.0, 0.0))
                .show(ctx, |ui| {
                    ui.with_layout(egui::Layout::left_to_right(egui::Align::TOP), |ui| {
                        ui.add_space(10.0);
                        ui.label(egui::RichText::new("Flora Compositor").color(egui::Color32::from_gray(200)).size(14.0));
                    });

                    for (idx, pos, _size, _is_focused, bar_id) in window_data {
                        let btn_radius = 6.0;
                        let spacing = 8.0;
                        let start_x_offset = 12.0;
                        let center_y_offset = (TITLE_BAR_HEIGHT as f32 / scale as f32) / 2.0;

                        let logical_pos = egui::pos2(pos.x as f32 / scale as f32, pos.y as f32 / scale as f32);

                        for i in 0..3 {
                            let (color, hover_color) = match i {
                                0 => (egui::Color32::from_rgb(255, 95, 87), egui::Color32::from_rgb(255, 120, 110)), 
                                1 => (egui::Color32::from_rgb(255, 189, 46), egui::Color32::from_rgb(255, 210, 100)),
                                2 => (egui::Color32::from_rgb(40, 201, 64), egui::Color32::from_rgb(80, 230, 100)),  
                                _ => (egui::Color32::GRAY, egui::Color32::LIGHT_GRAY),
                            };
                            
                            let center = egui::pos2(
                                logical_pos.x + start_x_offset + i as f32 * (btn_radius * 2.0 + spacing),
                                logical_pos.y + center_y_offset
                            );

                            let interaction_id = egui::Id::new(("btn", bar_id, i));
                            let rect = egui::Rect::from_center_size(center, egui::vec2(btn_radius * 2.2, btn_radius * 2.2));
                            let response = ui.interact(rect, interaction_id, egui::Sense::click());
                            
                            let final_color = if response.hovered() { hover_color } else { color };
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

    Ok((elements, pending_close))
}

pub fn get_output_scale(state: &FloraState) -> f64 {
    state.output.as_ref()
        .and_then(|o| o.current_scale().fractional_scale().into())
        .unwrap_or(1.0)
}

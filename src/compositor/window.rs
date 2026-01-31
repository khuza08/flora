use smithay::{
    wayland::shell::xdg::ToplevelSurface,
    utils::{Point, Physical, Logical},
};

pub const TITLE_BAR_HEIGHT: i32 = 30;

pub struct Window {
    pub toplevel: ToplevelSurface,
    pub location: Point<i32, Physical>,
    pub bar_id: smithay::backend::renderer::element::Id,
    pub bar_commit_counter: smithay::backend::renderer::utils::CommitCounter,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum HitRegion {
    TitleBar,
    Client { local_x: i32, local_y: i32 },
    None,
}

pub fn hit_test_window(
    pointer_logical: Point<f64, Logical>, 
    window_location_logical: Point<f64, Logical>, 
    surface_size: smithay::utils::Size<i32, Logical>
) -> HitRegion {
    let relative_x = pointer_logical.x - window_location_logical.x;
    let relative_y = pointer_logical.y - window_location_logical.y;
    
    if relative_x >= 0.0 && relative_x < surface_size.w as f64 {
        if relative_y >= 0.0 && relative_y < TITLE_BAR_HEIGHT as f64 {
            HitRegion::TitleBar
        } else if relative_y >= TITLE_BAR_HEIGHT as f64 && relative_y < (TITLE_BAR_HEIGHT + surface_size.h) as f64 {
            HitRegion::Client { 
                local_x: relative_x.round() as i32, 
                local_y: (relative_y - TITLE_BAR_HEIGHT as f64).round() as i32 
            }
        } else {
            HitRegion::None
        }
    } else {
        HitRegion::None
    }
}

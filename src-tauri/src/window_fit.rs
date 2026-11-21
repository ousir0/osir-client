use tauri::{LogicalSize, PhysicalPosition, PhysicalSize, WebviewWindow};

fn fit_dimension(current: u32, minimum: u32, default: u32, available: u32) -> u32 {
    if current < minimum.min(available) {
        default.max(minimum).min(available)
    } else {
        current.min(available)
    }
}

pub fn fit_to_work_area(window: &WebviewWindow, config: &tauri::utils::config::WindowConfig) {
    if window.is_minimized().unwrap_or(false)
        || window.is_maximized().unwrap_or(false)
        || window.is_fullscreen().unwrap_or(false)
    {
        return;
    }
    let (Ok(outer), Ok(inner)) = (window.outer_size(), window.inner_size()) else {
        return;
    };
    let Some(monitor) = window
        .current_monitor()
        .ok()
        .flatten()
        .or_else(|| window.primary_monitor().ok().flatten())
    else {
        return;
    };
    let work = monitor.work_area();
    if work.size.width == 0 || work.size.height == 0 {
        return;
    }
    let scale = window.scale_factor().unwrap_or(monitor.scale_factor());
    let minimum = LogicalSize::new(
        config.min_width.unwrap_or(0.0),
        config.min_height.unwrap_or(0.0),
    )
    .to_physical::<u32>(scale);
    let default = LogicalSize::new(config.width, config.height).to_physical::<u32>(scale);
    let frame_width = outer.width.saturating_sub(inner.width);
    let frame_height = outer.height.saturating_sub(inner.height);
    let width = fit_dimension(
        outer.width,
        minimum.width.saturating_add(frame_width),
        default.width.saturating_add(frame_width),
        work.size.width,
    );
    let height = fit_dimension(
        outer.height,
        minimum.height.saturating_add(frame_height),
        default.height.saturating_add(frame_height),
        work.size.height,
    );
    if (width, height) != (outer.width, outer.height) {
        if let Err(error) = window.set_size(PhysicalSize::new(
            width.saturating_sub(frame_width).max(1),
            height.saturating_sub(frame_height).max(1),
        )) {
            log::warn!("[Window] Could not restore window size: {error}");
            return;
        }
    }
    if let Ok(position) = window.outer_position() {
        let x = work.position.x;
        let y = work.position.y;
        let fitted = PhysicalPosition::new(
            position
                .x
                .clamp(x, x.saturating_add((work.size.width - width) as i32)),
            position
                .y
                .clamp(y, y.saturating_add((work.size.height - height) as i32)),
        );
        if fitted != position {
            if let Err(error) = window.set_position(fitted) {
                log::warn!("[Window] Could not restore window position: {error}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_and_minimum_account_for_display_scale() {
        for scale in [1.0, 1.25, 1.5, 2.0] {
            let minimum = LogicalSize::new(900.0, 600.0).to_physical::<u32>(scale);
            let default = LogicalSize::new(1100.0, 720.0).to_physical::<u32>(scale);
            assert_eq!(
                fit_dimension(30, minimum.height, default.height, 2160),
                default.height
            );
            assert_eq!(
                fit_dimension(minimum.width, minimum.width, default.width, 3840),
                minimum.width
            );
        }
    }

    #[test]
    fn small_work_areas_recover_without_repeated_resize() {
        for current in [30, 500, 1032] {
            assert_eq!(fit_dimension(current, 600, 720, 500), 500);
        }
    }
}

//! Armed spatial route segments for capture backends.
//!
//! Unlike [`input_event::display::DisplayLayout::exposed_segments`], these
//! intervals may include inward-facing edges derived from the shared
//! workspace. Backends that only observe outer contours still benefit when
//! the OS pointer can leave into a void (documented platform limitation).

use input_event::display::DisplayEdge;
use input_event::workspace::OutboundDir;
use std::sync::{Mutex, OnceLock};

use crate::Position;

fn armed_routes() -> &'static Mutex<Vec<ArmedRoute>> {
    static ROUTES: OnceLock<Mutex<Vec<ArmedRoute>>> = OnceLock::new();
    ROUTES.get_or_init(|| Mutex::new(Vec::new()))
}

/// Replace the process-wide armed spatial routes used by Win/macOS backends.
pub fn set_armed_routes(routes: Vec<ArmedRoute>) {
    if let Ok(mut guard) = armed_routes().lock() {
        *guard = routes;
    }
}

/// Snapshot of currently armed routes.
pub fn current_armed_routes() -> Vec<ArmedRoute> {
    armed_routes().lock().map(|g| g.clone()).unwrap_or_default()
}

/// One capture-armed transition interval in local device coordinates.
#[derive(Clone, Debug, PartialEq)]
pub struct ArmedRoute {
    pub edge: DisplayEdge,
    /// Inclusive/exclusive cross-axis range in local coordinates.
    pub start: i32,
    pub end: i32,
    /// Inclusive on-axis edge coordinate (last in-layout pixel).
    pub coordinate: i32,
    /// Legacy Position barrier this route maps to (for handle selection).
    pub position: Position,
}

impl ArmedRoute {
    pub fn outbound(&self) -> OutboundDir {
        match self.edge {
            DisplayEdge::Left => OutboundDir::Left,
            DisplayEdge::Right => OutboundDir::Right,
            DisplayEdge::Top => OutboundDir::Up,
            DisplayEdge::Bottom => OutboundDir::Down,
        }
    }

    pub fn contains_cross(&self, cross: i32) -> bool {
        cross >= self.start && cross < self.end
    }
}

/// Returns the first armed route crossed by motion from `prev` toward `curr`.
pub fn crossed_armed_route(
    routes: &[ArmedRoute],
    prev: (i32, i32),
    curr: (i32, i32),
) -> Option<&ArmedRoute> {
    let dx = curr.0 - prev.0;
    let dy = curr.1 - prev.1;
    let Some(dir) = OutboundDir::from_delta(f64::from(dx), f64::from(dy)) else {
        return None;
    };
    let edge = dir.edge();
    routes.iter().find(|route| {
        if route.edge != edge {
            return false;
        }
        let cross = match edge {
            DisplayEdge::Left | DisplayEdge::Right => prev.1,
            DisplayEdge::Top | DisplayEdge::Bottom => prev.0,
        };
        if !route.contains_cross(cross) {
            return false;
        }
        match edge {
            DisplayEdge::Left => curr.0 <= route.coordinate && prev.0 >= route.coordinate,
            DisplayEdge::Right => curr.0 >= route.coordinate && prev.0 <= route.coordinate,
            DisplayEdge::Top => curr.1 <= route.coordinate && prev.1 >= route.coordinate,
            DisplayEdge::Bottom => curr.1 >= route.coordinate && prev.1 <= route.coordinate,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_partial_and_inward_edges() {
        let routes = vec![
            ArmedRoute {
                edge: DisplayEdge::Right,
                start: 200,
                end: 800,
                coordinate: 1919,
                position: Position::Right,
            },
            // Inward-facing top of a lower display in a U.
            ArmedRoute {
                edge: DisplayEdge::Bottom,
                start: 400,
                end: 1200,
                coordinate: 399,
                position: Position::Bottom,
            },
        ];
        assert!(crossed_armed_route(&routes, (1910, 100), (1925, 100)).is_none());
        assert!(crossed_armed_route(&routes, (1910, 500), (1925, 500)).is_some());
        assert!(
            crossed_armed_route(&routes, (800, 390), (800, 410))
                .is_some_and(|r| r.edge == DisplayEdge::Bottom)
        );
    }
}

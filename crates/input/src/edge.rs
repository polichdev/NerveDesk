use tracing::info;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActiveLocation {
    Host,
    Client,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScreenEdge {
    Left,
    Right,
    Top,
    Bottom,
}

#[derive(Debug, Clone)]
pub struct EdgeConfig {
    pub host_width: i32,
    pub host_height: i32,
    pub client_edge: ScreenEdge,
    pub trigger_threshold: i32,
}

impl Default for EdgeConfig {
    fn default() -> Self {
        Self {
            host_width: 1920,
            host_height: 1080,
            client_edge: ScreenEdge::Right,
            trigger_threshold: 2,
        }
    }
}

pub struct EdgeDetector {
    config: EdgeConfig,
    active_location: ActiveLocation,
    virtual_client_x: i32,
    virtual_client_y: i32,
}

impl EdgeDetector {
    pub fn new(config: EdgeConfig) -> Self {
        Self {
            config,
            active_location: ActiveLocation::Host,
            virtual_client_x: 0,
            virtual_client_y: 0,
        }
    }

    pub fn evaluate_motion(
        &mut self,
        current_x: i32,
        current_y: i32,
        dx: i32,
        dy: i32,
    ) -> Option<ActiveLocation> {
        match self.active_location {
            ActiveLocation::Host => {
                let crossed = match self.config.client_edge {
                    ScreenEdge::Right => {
                        current_x >= (self.config.host_width - self.config.trigger_threshold)
                            && dx > 0
                    }
                    ScreenEdge::Left => {
                        current_x <= self.config.trigger_threshold && dx < 0
                    }
                    ScreenEdge::Bottom => {
                        current_y >= (self.config.host_height - self.config.trigger_threshold)
                            && dy > 0
                    }
                    ScreenEdge::Top => {
                        current_y <= self.config.trigger_threshold && dy < 0
                    }
                };

                if crossed {
                    info!(
                        "Cursor crossed {:?} edge into Client display space (x={}, y={})",
                        self.config.client_edge, current_x, current_y
                    );
                    self.active_location = ActiveLocation::Client;
                    self.virtual_client_x = 0;
                    self.virtual_client_y = current_y;
                    return Some(ActiveLocation::Client);
                }
            }
            ActiveLocation::Client => {
                self.virtual_client_x += dx;
                self.virtual_client_y += dy;

                let returned = match self.config.client_edge {
                    ScreenEdge::Right => self.virtual_client_x < 0,
                    ScreenEdge::Left => self.virtual_client_x > 0,
                    ScreenEdge::Bottom => self.virtual_client_y < 0,
                    ScreenEdge::Top => self.virtual_client_y > 0,
                };

                if returned {
                    info!("Cursor returned back from Client to Host screen");
                    self.active_location = ActiveLocation::Host;
                    return Some(ActiveLocation::Host);
                }
            }
        }

        None
    }

    pub fn set_location(&mut self, location: ActiveLocation) {
        self.active_location = location;
    }

    pub fn active_location(&self) -> ActiveLocation {
        self.active_location
    }

    pub fn config(&self) -> &EdgeConfig {
        &self.config
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_edge_crossing_right() {
        let config = EdgeConfig {
            host_width: 1920,
            host_height: 1080,
            client_edge: ScreenEdge::Right,
            trigger_threshold: 2,
        };
        let mut detector = EdgeDetector::new(config);

        assert_eq!(detector.evaluate_motion(500, 500, 10, 0), None);
        assert_eq!(detector.active_location(), ActiveLocation::Host);

        let switch = detector.evaluate_motion(1919, 500, 5, 0);
        assert_eq!(switch, Some(ActiveLocation::Client));
        assert_eq!(detector.active_location(), ActiveLocation::Client);

        assert_eq!(detector.evaluate_motion(1919, 500, 50, 0), None);

        let return_switch = detector.evaluate_motion(1919, 500, -60, 0);
        assert_eq!(return_switch, Some(ActiveLocation::Host));
        assert_eq!(detector.active_location(), ActiveLocation::Host);
    }
}

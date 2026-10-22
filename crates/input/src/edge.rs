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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransitionResult {
    CrossedToClient { client_x: i32, client_y: i32 },
    ReturnedToHost { return_x: i32, return_y: i32 },
}

#[derive(Debug, Clone)]
pub struct EdgeConfig {
    pub host_width: i32,
    pub host_height: i32,
    pub client_width: i32,
    pub client_height: i32,
    pub client_edge: ScreenEdge,
    pub peer_edge: ScreenEdge,
    pub trigger_threshold: i32,
}

impl Default for EdgeConfig {
    fn default() -> Self {
        Self {
            host_width: 1920,
            host_height: 1080,
            client_width: 1920,
            client_height: 1080,
            client_edge: ScreenEdge::Right,
            peer_edge: ScreenEdge::Left,
            trigger_threshold: 4,
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

    pub fn set_dimensions(&mut self, host_w: i32, host_h: i32, client_w: i32, client_h: i32) {
        if host_w > 0 {
            self.config.host_width = host_w;
        }
        if host_h > 0 {
            self.config.host_height = host_h;
        }
        if client_w > 0 {
            self.config.client_width = client_w;
        }
        if client_h > 0 {
            self.config.client_height = client_h;
        }
    }

    pub fn set_peer_edge(&mut self, edge: ScreenEdge) {
        self.config.peer_edge = edge;
    }

    pub fn evaluate_motion(
        &mut self,
        current_x: i32,
        current_y: i32,
        dx: i32,
        dy: i32,
    ) -> Option<TransitionResult> {
        match self.active_location {
            ActiveLocation::Host => {
                let crossed = match self.config.client_edge {
                    ScreenEdge::Right => {
                        current_x >= (self.config.host_width - self.config.trigger_threshold)
                            && dx > 0
                    }
                    ScreenEdge::Left => current_x <= self.config.trigger_threshold && dx < 0,
                    ScreenEdge::Bottom => {
                        current_y >= (self.config.host_height - self.config.trigger_threshold)
                            && dy > 0
                    }
                    ScreenEdge::Top => current_y <= self.config.trigger_threshold && dy < 0,
                };

                if crossed {
                    info!(
                        "Cursor crossed {:?} edge into Client display space (x={}, y={})",
                        self.config.client_edge, current_x, current_y
                    );
                    self.active_location = ActiveLocation::Client;

                    let (source_along, source_span) = match self.config.client_edge {
                        ScreenEdge::Left | ScreenEdge::Right => {
                            (current_y, self.config.host_height)
                        }
                        ScreenEdge::Top | ScreenEdge::Bottom => (current_x, self.config.host_width),
                    };
                    let target_span = match self.config.peer_edge {
                        ScreenEdge::Left | ScreenEdge::Right => self.config.client_height,
                        ScreenEdge::Top | ScreenEdge::Bottom => self.config.client_width,
                    };
                    let along =
                        (source_along * target_span / source_span).clamp(0, target_span - 1);
                    let (cx, cy) = match self.config.peer_edge {
                        ScreenEdge::Left => {
                            let cx = 25;
                            (cx, along)
                        }
                        ScreenEdge::Right => {
                            let cx = (self.config.client_width - 25).max(0);
                            (cx, along)
                        }
                        ScreenEdge::Bottom => {
                            let cy = (self.config.client_height - 25).max(0);
                            (along, cy)
                        }
                        ScreenEdge::Top => {
                            let cy = 25;
                            (along, cy)
                        }
                    };

                    self.virtual_client_x = cx;
                    self.virtual_client_y = cy;

                    return Some(TransitionResult::CrossedToClient {
                        client_x: cx,
                        client_y: cy,
                    });
                }
            }
            ActiveLocation::Client => {
                self.virtual_client_x += dx;
                self.virtual_client_y += dy;
                let crossed = match self.config.peer_edge {
                    ScreenEdge::Left => self.virtual_client_x <= 0 && dx < 0,
                    ScreenEdge::Right => {
                        self.virtual_client_x >= self.config.client_width - 1 && dx > 0
                    }
                    ScreenEdge::Top => self.virtual_client_y <= 0 && dy < 0,
                    ScreenEdge::Bottom => {
                        self.virtual_client_y >= self.config.client_height - 1 && dy > 0
                    }
                };
                self.virtual_client_x =
                    self.virtual_client_x.clamp(0, self.config.client_width - 1);
                self.virtual_client_y = self
                    .virtual_client_y
                    .clamp(0, self.config.client_height - 1);
                if crossed {
                    info!(
                        "Cursor returned from Client via {:?} edge",
                        self.config.peer_edge
                    );
                    self.active_location = ActiveLocation::Host;
                    let inset = self.config.trigger_threshold + 25;
                    let (peer_along, peer_span) = match self.config.peer_edge {
                        ScreenEdge::Left | ScreenEdge::Right => {
                            (self.virtual_client_y, self.config.client_height)
                        }
                        ScreenEdge::Top | ScreenEdge::Bottom => {
                            (self.virtual_client_x, self.config.client_width)
                        }
                    };
                    let host_span = match self.config.client_edge {
                        ScreenEdge::Left | ScreenEdge::Right => self.config.host_height,
                        ScreenEdge::Top | ScreenEdge::Bottom => self.config.host_width,
                    };
                    let along = (peer_along * host_span / peer_span).clamp(0, host_span - 1);
                    let (return_x, return_y) = match self.config.client_edge {
                        ScreenEdge::Left => (inset, along),
                        ScreenEdge::Right => (self.config.host_width - inset, along),
                        ScreenEdge::Top => (along, inset),
                        ScreenEdge::Bottom => (along, self.config.host_height - inset),
                    };
                    return Some(TransitionResult::ReturnedToHost { return_x, return_y });
                }
            }
        }

        None
    }

    pub fn virtual_client_pos(&self) -> (i32, i32) {
        (self.virtual_client_x, self.virtual_client_y)
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
            client_width: 1920,
            client_height: 1080,
            client_edge: ScreenEdge::Right,
            peer_edge: ScreenEdge::Left,
            trigger_threshold: 4,
        };
        let mut detector = EdgeDetector::new(config);

        assert_eq!(detector.evaluate_motion(500, 500, 10, 0), None);
        assert_eq!(detector.active_location(), ActiveLocation::Host);

        let switch = detector.evaluate_motion(1919, 500, 5, 0);
        assert!(matches!(
            switch,
            Some(TransitionResult::CrossedToClient { .. })
        ));
        assert_eq!(detector.active_location(), ActiveLocation::Client);

        assert_eq!(detector.evaluate_motion(1919, 500, 50, 0), None);
        assert_eq!(detector.evaluate_motion(1919, 500, 0, 50), None);
        assert_eq!(detector.evaluate_motion(1919, 500, 0, -50), None);

        let return_switch = detector.evaluate_motion(1919, 500, -200, 0);
        assert!(matches!(
            return_switch,
            Some(TransitionResult::ReturnedToHost { .. })
        ));
        assert_eq!(detector.active_location(), ActiveLocation::Host);
    }

    #[test]
    fn uses_independent_exit_edges_on_each_device() {
        let mut detector = EdgeDetector::new(EdgeConfig {
            host_width: 1920,
            host_height: 1080,
            client_width: 1600,
            client_height: 900,
            client_edge: ScreenEdge::Left,
            peer_edge: ScreenEdge::Top,
            trigger_threshold: 4,
        });
        assert_eq!(
            detector.evaluate_motion(2, 540, -5, 0),
            Some(TransitionResult::CrossedToClient {
                client_x: 800,
                client_y: 25,
            })
        );
        assert_eq!(detector.evaluate_motion(2, 540, 100, 0), None);
        assert_eq!(
            detector.evaluate_motion(2, 540, 0, -30),
            Some(TransitionResult::ReturnedToHost {
                return_x: 29,
                return_y: 607,
            })
        );
    }
}

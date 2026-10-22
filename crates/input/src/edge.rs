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

                    let (cx, cy) = match self.config.client_edge {
                        ScreenEdge::Right => {
                            let cx = 25;
                            let cy = (current_y * self.config.client_height
                                / self.config.host_height)
                                .clamp(0, self.config.client_height - 1);
                            (cx, cy)
                        }
                        ScreenEdge::Left => {
                            let cx = (self.config.client_width - 25).max(0);
                            let cy = (current_y * self.config.client_height
                                / self.config.host_height)
                                .clamp(0, self.config.client_height - 1);
                            (cx, cy)
                        }
                        ScreenEdge::Top => {
                            let cx = (current_x * self.config.client_width
                                / self.config.host_width)
                                .clamp(0, self.config.client_width - 1);
                            let cy = (self.config.client_height - 25).max(0);
                            (cx, cy)
                        }
                        ScreenEdge::Bottom => {
                            let cx = (current_x * self.config.client_width
                                / self.config.host_width)
                                .clamp(0, self.config.client_width - 1);
                            let cy = 25;
                            (cx, cy)
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

                match self.config.client_edge {
                    ScreenEdge::Right => {
                        self.virtual_client_y = self
                            .virtual_client_y
                            .clamp(0, self.config.client_height - 1);
                        if self.virtual_client_x > self.config.client_width - 1 {
                            self.virtual_client_x = self.config.client_width - 1;
                        }
                        if self.virtual_client_x <= 0 && dx < 0 {
                            info!("Cursor returned back from Client to Host screen via Left edge");
                            self.active_location = ActiveLocation::Host;
                            let return_x =
                                self.config.host_width - self.config.trigger_threshold - 25;
                            let return_y = (self.virtual_client_y * self.config.host_height
                                / self.config.client_height)
                                .clamp(0, self.config.host_height - 1);
                            return Some(TransitionResult::ReturnedToHost { return_x, return_y });
                        }
                    }
                    ScreenEdge::Left => {
                        self.virtual_client_y = self
                            .virtual_client_y
                            .clamp(0, self.config.client_height - 1);
                        if self.virtual_client_x < 0 {
                            self.virtual_client_x = 0;
                        }
                        if self.virtual_client_x >= self.config.client_width - 1 && dx > 0 {
                            info!("Cursor returned back from Client to Host screen via Right edge");
                            self.active_location = ActiveLocation::Host;
                            let return_x = self.config.trigger_threshold + 25;
                            let return_y = (self.virtual_client_y * self.config.host_height
                                / self.config.client_height)
                                .clamp(0, self.config.host_height - 1);
                            return Some(TransitionResult::ReturnedToHost { return_x, return_y });
                        }
                    }
                    ScreenEdge::Top => {
                        self.virtual_client_x =
                            self.virtual_client_x.clamp(0, self.config.client_width - 1);
                        if self.virtual_client_y < 0 {
                            self.virtual_client_y = 0;
                        }
                        if self.virtual_client_y >= self.config.client_height - 1 && dy > 0 {
                            info!(
                                "Cursor returned back from Client to Host screen via Bottom edge"
                            );
                            self.active_location = ActiveLocation::Host;
                            let return_x = (self.virtual_client_x * self.config.host_width
                                / self.config.client_width)
                                .clamp(0, self.config.host_width - 1);
                            let return_y = self.config.trigger_threshold + 25;
                            return Some(TransitionResult::ReturnedToHost { return_x, return_y });
                        }
                    }
                    ScreenEdge::Bottom => {
                        self.virtual_client_x =
                            self.virtual_client_x.clamp(0, self.config.client_width - 1);
                        if self.virtual_client_y > self.config.client_height - 1 {
                            self.virtual_client_y = self.config.client_height - 1;
                        }
                        if self.virtual_client_y <= 0 && dy < 0 {
                            info!("Cursor returned back from Client to Host screen via Top edge");
                            self.active_location = ActiveLocation::Host;
                            let return_x = (self.virtual_client_x * self.config.host_width
                                / self.config.client_width)
                                .clamp(0, self.config.host_width - 1);
                            let return_y =
                                self.config.host_height - self.config.trigger_threshold - 25;
                            return Some(TransitionResult::ReturnedToHost { return_x, return_y });
                        }
                    }
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
}

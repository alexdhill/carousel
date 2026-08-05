use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum GuideAxis {
    Horizontal,
    Vertical,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Guide {
    pub axis: GuideAxis,
    pub pos: f64,
}

impl Guide {

    pub fn new(axis: GuideAxis, pos: f64) -> Self {
        Self { axis, pos }
    }
}

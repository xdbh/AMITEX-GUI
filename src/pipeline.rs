pub(crate) mod loads;
pub(crate) mod runner;
mod simulation;

use crate::preproc::algorithm::AlgorithmType;

pub(crate) struct SimulationConfig {
    pub(crate) name: String,
    pub(crate) mode: SimulationModes,
    pub(crate) algorithm: AlgorithmType,
}

#[derive(Default, PartialEq)]
pub(crate) enum SimulationModes {
    #[default]
    ElasticHomogenization,
}

impl SimulationModes {
    pub(crate) fn label(&self) -> &'static str {
        match self {
            SimulationModes::ElasticHomogenization => "Elastic homogenization",
        }
    }
}

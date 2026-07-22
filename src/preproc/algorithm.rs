#[derive(Default, Debug, PartialEq)]
pub(crate) enum AlgorithmType {
    #[default]
    BasicScheme,
}

/// NOT IN USE, SETTINGS FILE CHOSEN AND PASSED DIRECTLY TO AMITEX
impl AlgorithmType {
    fn label(&self) -> &'static str {
        match self {
            AlgorithmType::BasicScheme => "Basic Scheme (Default)",
        }
    }
}

#[derive(Default, Debug, PartialEq)]
pub(crate) enum FilterType {
    #[default]
    Hexa,
    NoFilter,
    Octa,
}

impl FilterType {
    fn label(&self) -> &'static str {
        match self {
            FilterType::Hexa => "Hexa (Default)",
            FilterType::Octa => "Octa",
            FilterType::NoFilter => "NoFilter",
        }
    }
}

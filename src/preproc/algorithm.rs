//! In-GUI algorithm-XML editor, replacing the old browse-to-a-file workflow. Schema per
//! AMITEX's user guide (`user_guide/algorithm.html`), which — unlike the material laws in
//! `preproc::materials` — is fully documented online.

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum AlgorithmType {
    BasicScheme,
}

impl Default for AlgorithmType {
    fn default() -> Self {
        AlgorithmType::BasicScheme
    }
}

impl AlgorithmType {
    pub(crate) fn label(&self) -> &'static str {
        match self {
            AlgorithmType::BasicScheme => "Basic Scheme",
        }
    }

    fn xml_value(&self) -> &'static str {
        match self {
            AlgorithmType::BasicScheme => "Basic_Scheme",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum FilterType {
    Hexa,
    NoFilter,
    Octa,
}

impl Default for FilterType {
    fn default() -> Self {
        FilterType::Hexa
    }
}

impl FilterType {
    pub(crate) fn label(&self) -> &'static str {
        match self {
            FilterType::Hexa => "Hexa (default)",
            FilterType::Octa => "Octa",
            FilterType::NoFilter => "No filter",
        }
    }

    fn xml_value(&self) -> &'static str {
        match self {
            FilterType::Hexa => "hexa",
            FilterType::Octa => "octa",
            FilterType::NoFilter => "no_filter",
        }
    }
}

/// Everything needed to generate `algo.xml`. Field defaults match AMITEX's own documented
/// defaults, so an unedited `AlgorithmSettings` produces the same effective behavior as the
/// legacy `algo_default.xml` (see `20260716142000/algo_default.xml`).
pub(crate) struct AlgorithmSettings {
    pub(crate) algorithm_type: AlgorithmType,
    pub(crate) filter: FilterType,
    /// `None` = AMITEX's "Default" (1e-4); `Some` must be a positive value `< 1e-3`.
    pub(crate) convergence_criterion: Option<f64>,
    pub(crate) convergence_acceleration: bool,
    /// Only meaningful (written to XML) when `convergence_acceleration` is true.
    pub(crate) mod_acv: u32,
    pub(crate) nitermax: u32,
}

impl Default for AlgorithmSettings {
    fn default() -> Self {
        Self {
            algorithm_type: AlgorithmType::default(),
            filter: FilterType::default(),
            convergence_criterion: None,
            convergence_acceleration: true,
            mod_acv: 3,
            nitermax: 1000,
        }
    }
}

impl AlgorithmSettings {
    /// Generates `algo.xml`, matching the structure/style of the legacy hand-written
    /// `algo_default.xml`. `Small_Perturbations` is always `true` (small-strain) rather than
    /// a user-editable option: this app's stiffness-matrix extraction
    /// (`postproc::moduli`) assumes a linear small-strain response — see the doc comment on
    /// `preproc::materials`'s large-strain law variants for why those aren't offered either.
    pub(crate) fn generate_xml(&self) -> String {
        let mut xml = String::new();
        xml.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
        xml.push_str("<Algorithm_Parameters>\n\n");

        xml.push_str(&format!("    <Algorithm Type=\"{}\">\n", self.algorithm_type.xml_value()));
        let criterion = self
            .convergence_criterion
            .map(|value| value.to_string())
            .unwrap_or_else(|| "Default".to_string());
        xml.push_str(&format!("        <Convergence_Criterion Value=\"{criterion}\"/>\n"));
        xml.push_str(&format!(
            "        <Convergence_Acceleration Value=\"{}\" modACV=\"{}\"/>\n",
            self.convergence_acceleration, self.mod_acv
        ));
        xml.push_str(&format!("        <Nitermax Value=\"{}\"/>\n", self.nitermax));
        xml.push_str("    </Algorithm>\n\n");

        xml.push_str("    <Mechanics>\n");
        xml.push_str(&format!("        <Filter Type=\"{}\"/>\n", self.filter.xml_value()));
        xml.push_str("        <Small_Perturbations Value=\"true\"/>\n");
        xml.push_str("    </Mechanics>\n");

        xml.push_str("</Algorithm_Parameters>\n");
        xml
    }
}

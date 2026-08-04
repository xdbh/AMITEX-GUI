//! In-GUI material law editor and `mat.xml` generation, replacing the old
//! browse-to-a-file workflow. Coefficient specs below were read directly from AMITEX's
//! Fortran source (`libAmitex/src/materiaux/*.f90`) rather than the public docs, which only
//! document `elasiso`'s coefficients — the rest are documented only in that source, which
//! isn't publicly hosted.
//!
//! Only `ElasticIsotropic` and `ElasticOrthotropic` are wired up to generate real XML. Both
//! produce an instantaneous linear `STRESS = C:STRAIN` response, which is what this app's
//! elastic-homogenization pipeline actually needs (6 unit-strain cases -> read back stress ->
//! assemble a stiffness matrix, see `postproc::moduli`). The other 7 native laws are named
//! here (so they're visible as a roadmap) but deliberately not selectable yet:
//! - `ElasticIsotropicEigenstrain`/`ElasticIsotropicLargeStrain`/`ElasticOrthotropicLargeStrain`:
//!   same underlying elasticity, but a nonzero eigenstrain or finite-strain kinematics would
//!   silently invalidate `postproc::moduli`'s raw-stress-column extraction.
//! - `Thermoelastic`/`ThermoelasticExternalParam`: need a temperature/external-field loading
//!   value this app's `char.xml` generator (`pipeline::loads`) doesn't produce.
//! - `ImposedStress`: not a real constitutive response (outputs a fixed stress regardless of
//!   applied strain) — there's no modulus to homogenize.
//! - `ViscoelasticMaxwell`: needs genuine time-stepped loading history, not one static
//!   unit-strain case per direction.

use crate::postproc::vtkio::{distinct_sorted_values, VtkGrid};
use anyhow::bail;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum LawKind {
    ElasticIsotropic,
    ElasticOrthotropic,
    ElasticIsotropicEigenstrain,
    ElasticIsotropicLargeStrain,
    ElasticOrthotropicLargeStrain,
    Thermoelastic,
    ThermoelasticExternalParam,
    ImposedStress,
    ViscoelasticMaxwell,
}

impl LawKind {
    pub(crate) const ALL: [LawKind; 9] = [
        LawKind::ElasticIsotropic,
        LawKind::ElasticOrthotropic,
        LawKind::ElasticIsotropicEigenstrain,
        LawKind::ElasticIsotropicLargeStrain,
        LawKind::ElasticOrthotropicLargeStrain,
        LawKind::Thermoelastic,
        LawKind::ThermoelasticExternalParam,
        LawKind::ImposedStress,
        LawKind::ViscoelasticMaxwell,
    ];

    /// Human-readable name shown in the GUI's law picker.
    pub(crate) fn label(&self) -> &'static str {
        match self {
            LawKind::ElasticIsotropic => "Elastic — isotropic",
            LawKind::ElasticOrthotropic => "Elastic — orthotropic",
            LawKind::ElasticIsotropicEigenstrain => "Elastic — isotropic + eigenstrain (not yet implemented)",
            LawKind::ElasticIsotropicLargeStrain => "Elastic — isotropic, large strain (not yet implemented)",
            LawKind::ElasticOrthotropicLargeStrain => "Elastic — orthotropic, large strain (not yet implemented)",
            LawKind::Thermoelastic => "Thermoelastic — isotropic (not yet implemented)",
            LawKind::ThermoelasticExternalParam => "Thermoelastic — external parameter (not yet implemented)",
            LawKind::ImposedStress => "Imposed stress (not yet implemented)",
            LawKind::ViscoelasticMaxwell => "Viscoelastic — generalized Maxwell (not yet implemented)",
        }
    }

    /// The AMITEX `Law="..."` attribute value (`libAmitex/src/materiaux/<name>.f90`).
    pub(crate) fn amitex_name(&self) -> &'static str {
        match self {
            LawKind::ElasticIsotropic => "elasiso",
            LawKind::ElasticOrthotropic => "elasaniso",
            LawKind::ElasticIsotropicEigenstrain => "elasiso_eigs",
            LawKind::ElasticIsotropicLargeStrain => "elasiso_GD",
            LawKind::ElasticOrthotropicLargeStrain => "elasaniso_GD",
            LawKind::Thermoelastic => "thermoelasiso",
            LawKind::ThermoelasticExternalParam => "paramextelasiso",
            LawKind::ImposedStress => "contrainte_imposee",
            LawKind::ViscoelasticMaxwell => "viscoelas_maxwell",
        }
    }

    pub(crate) fn implemented(&self) -> bool {
        matches!(self, LawKind::ElasticIsotropic | LawKind::ElasticOrthotropic)
    }
}

impl Default for LawKind {
    fn default() -> Self {
        LawKind::ElasticIsotropic
    }
}

/// `elasiso` (isotropic elasticity) coefficients, entered as engineering constants — matches
/// what `postproc::moduli` reports back after a run — and converted to the Lamé coefficients
/// `elasiso.f90` actually takes (`PROPS(1)=lambda, PROPS(2)=mu`) at XML-generation time.
#[derive(Clone, Copy)]
pub(crate) struct IsotropicCoeffs {
    pub(crate) e: f64,
    pub(crate) nu: f64,
}

impl Default for IsotropicCoeffs {
    fn default() -> Self {
        Self { e: 0.0, nu: 0.0 }
    }
}

impl IsotropicCoeffs {
    /// Standard isotropic-elasticity conversion from engineering constants to Lamé
    /// coefficients: `lambda = E*nu / ((1+nu)*(1-2*nu))`, `mu = E / (2*(1+nu))`.
    fn lame(&self) -> (f64, f64) {
        let lambda = self.e * self.nu / ((1.0 + self.nu) * (1.0 - 2.0 * self.nu));
        let mu = self.e / (2.0 * (1.0 + self.nu));
        (lambda, mu)
    }
}

/// `elasaniso` (orthotropic elasticity) coefficients: `PROPS(1..9)` are the upper triangle of
/// the local-basis stiffness matrix, `PROPS(10:12)`/`PROPS(13:15)` are the two local basis
/// vectors (`e1`, `e2` — `elasaniso.f90` normalizes `e1`, then Gram-Schmidt-orthogonalizes and
/// normalizes `e2` against it, then derives `e3` itself).
#[derive(Clone, Copy)]
pub(crate) struct OrthotropicCoeffs {
    pub(crate) c11: f64,
    pub(crate) c12: f64,
    pub(crate) c13: f64,
    pub(crate) c22: f64,
    pub(crate) c23: f64,
    pub(crate) c33: f64,
    pub(crate) c44: f64,
    pub(crate) c55: f64,
    pub(crate) c66: f64,
    pub(crate) e1: [f64; 3],
    pub(crate) e2: [f64; 3],
}

impl Default for OrthotropicCoeffs {
    fn default() -> Self {
        Self {
            c11: 0.0,
            c12: 0.0,
            c13: 0.0,
            c22: 0.0,
            c23: 0.0,
            c33: 0.0,
            c44: 0.0,
            c55: 0.0,
            c66: 0.0,
            // Aligned with the global axes by default — a valid, orthonormal starting basis.
            e1: [1.0, 0.0, 0.0],
            e2: [0.0, 1.0, 0.0],
        }
    }
}

/// One `<Material numM="...">` block's worth of GUI state: which law, and (regardless of
/// which is currently selected) storage for both implemented laws' coefficients, so
/// switching the law dropdown back and forth doesn't discard what was typed in.
pub(crate) struct MaterialEntry {
    /// AMITEX's 1-based material number — see `detect_material_ids`.
    pub(crate) num_m: usize,
    /// The raw value this material's voxels have in `material_id_vtk`, shown alongside
    /// `num_m` so the mapping back to the source file is visible.
    pub(crate) vtk_id: f64,
    pub(crate) law: LawKind,
    pub(crate) isotropic: IsotropicCoeffs,
    pub(crate) orthotropic: OrthotropicCoeffs,
}

/// Distinct material IDs in `grid`, paired with the AMITEX `numM` each maps to, in ascending
/// `numM` order. Enforces AMITEX's actual numbering rule (`read_geom.f90`): the minimum ID
/// must be exactly 0 or 1 (re-indexed to 1-based if it's 0), and IDs must be contiguous — a
/// gap would mean `mat.xml` needs a `<Material>` block for every integer in the gap too
/// (`material_mod.f90` hard-errors if the `<Material>` count doesn't exactly equal
/// `max(numM)`), which isn't something a per-material GUI editor can sensibly ask for.
pub(crate) fn detect_material_ids(grid: &VtkGrid) -> anyhow::Result<Vec<(f64, usize)>> {
    let ids = distinct_sorted_values(&grid.data);
    let Some(&min) = ids.first() else {
        bail!("material ID VTK has no cell data");
    };
    if min != 0.0 && min != 1.0 {
        bail!("material IDs must start at 0 or 1 (AMITEX requirement) — found minimum {min}");
    }
    if let Some(&bad) = ids.iter().find(|v| v.fract() != 0.0) {
        bail!("material IDs must be integers — found {bad}");
    }
    let max = *ids.last().unwrap();
    let expected_count = (max - min) as usize + 1;
    if ids.len() != expected_count {
        bail!(
            "material IDs must be contiguous with no gaps — found {} distinct value(s) but the \
             range {min}..={max} implies {expected_count}; AMITEX requires a <Material> block \
             for every number in that range, including gaps",
            ids.len()
        );
    }
    let offset = if min == 0.0 { 1 } else { 0 };
    Ok(ids.into_iter().map(|id| (id, id as usize + offset)).collect())
}

/// Reference-medium Lamé coefficients for the FFT scheme's convergence (not a real material —
/// affects iteration count, not physical accuracy). Follows AMITEX's documented rule
/// (Moulinec): `X0 = (min(X) + max(X)) / 2` for `X` = lambda or mu, across all materials.
/// Orthotropic materials don't have a single lambda/mu, so their contribution uses an
/// approximation (average off-diagonal term standing in for lambda, average shear term
/// standing in for mu) — this only affects convergence speed, not the computed result.
fn reference_lambda_mu(materials: &[MaterialEntry]) -> (f64, f64) {
    let mut lambdas = Vec::new();
    let mut mus = Vec::new();
    for m in materials {
        match m.law {
            LawKind::ElasticIsotropic => {
                let (lambda, mu) = m.isotropic.lame();
                lambdas.push(lambda);
                mus.push(mu);
            }
            LawKind::ElasticOrthotropic => {
                let c = &m.orthotropic;
                lambdas.push((c.c12 + c.c13 + c.c23) / 3.0);
                mus.push((c.c44 + c.c55 + c.c66) / 3.0);
            }
            _ => {}
        }
    }
    let bounds = |values: &[f64]| {
        let min = values.iter().cloned().fold(f64::INFINITY, f64::min);
        let max = values.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
        (min + max) / 2.0
    };
    (bounds(&lambdas), bounds(&mus))
}

/// Generates `mat.xml` from the entered materials, matching the structure/style of the
/// legacy hand-written `mat.xml` (see `20260716142000/mat.xml`): a `Reference_Material`
/// followed by one commented `<Material numM="...">` block per material, in `numM` order.
pub(crate) fn generate_mat_xml(materials: &[MaterialEntry]) -> anyhow::Result<String> {
    if materials.is_empty() {
        bail!("no materials defined — select a Material ID VTK file first");
    }
    if let Some(unimplemented) = materials.iter().find(|m| !m.law.implemented()) {
        bail!(
            "material {} uses \"{}\", which isn't implemented yet — pick a different law",
            unimplemented.num_m,
            unimplemented.law.label()
        );
    }

    let mut sorted: Vec<&MaterialEntry> = materials.iter().collect();
    sorted.sort_by_key(|m| m.num_m);
    for (i, m) in sorted.iter().enumerate() {
        if m.num_m != i + 1 {
            bail!(
                "material numbers must be contiguous starting at 1 — found numM={} at position {}",
                m.num_m,
                i + 1
            );
        }
    }

    let (lambda0, mu0) = reference_lambda_mu(materials);

    let mut xml = String::new();
    xml.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    xml.push_str("<Materials>\n\n");
    xml.push_str("<!-- REFERENCE MATERIAL (FFT scheme convergence only, not a real material) -->\n");
    xml.push_str(&format!("<Reference_Material Lambda0=\"{lambda0}\" Mu0=\"{mu0}\" />\n\n"));

    for m in sorted {
        xml.push_str(&format!("<!-- MATERIAL {} -->\n", m.num_m));
        match m.law {
            LawKind::ElasticIsotropic => {
                let (lambda, mu) = m.isotropic.lame();
                xml.push_str(&format!(
                    "<Material numM=\"{}\" Lib=\"\" Law=\"{}\">\n",
                    m.num_m,
                    m.law.amitex_name()
                ));
                xml.push_str(&format!(
                    "    <Coeff Index=\"1\" Type=\"Constant\" Value=\"{lambda}\"/> <!-- lambda -->\n"
                ));
                xml.push_str(&format!(
                    "    <Coeff Index=\"2\" Type=\"Constant\" Value=\"{mu}\"/> <!-- mu -->\n"
                ));
                xml.push_str("</Material>\n\n");
            }
            LawKind::ElasticOrthotropic => {
                let c = &m.orthotropic;
                xml.push_str(&format!(
                    "<Material numM=\"{}\" Lib=\"\" Law=\"{}\">\n",
                    m.num_m,
                    m.law.amitex_name()
                ));
                let stiffness = [
                    ("C11", c.c11),
                    ("C12", c.c12),
                    ("C13", c.c13),
                    ("C22", c.c22),
                    ("C23", c.c23),
                    ("C33", c.c33),
                    ("C44", c.c44),
                    ("C55", c.c55),
                    ("C66", c.c66),
                ];
                for (i, (label, value)) in stiffness.iter().enumerate() {
                    xml.push_str(&format!(
                        "    <Coeff Index=\"{}\" Type=\"Constant\" Value=\"{value}\"/> <!-- {label} -->\n",
                        i + 1
                    ));
                }
                for (i, value) in c.e1.iter().enumerate() {
                    xml.push_str(&format!(
                        "    <Coeff Index=\"{}\" Type=\"Constant\" Value=\"{value}\"/> <!-- e1[{i}] -->\n",
                        10 + i
                    ));
                }
                for (i, value) in c.e2.iter().enumerate() {
                    xml.push_str(&format!(
                        "    <Coeff Index=\"{}\" Type=\"Constant\" Value=\"{value}\"/> <!-- e2[{i}] -->\n",
                        13 + i
                    ));
                }
                xml.push_str("</Material>\n\n");
            }
            // Filtered out above.
            _ => unreachable!(),
        }
    }
    xml.push_str("</Materials>\n");
    Ok(xml)
}

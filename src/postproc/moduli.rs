use anyhow::{bail, Context};
use std::path::Path;

pub(crate) struct ElasticModuli {
    /// 6x6 stiffness matrix.
    pub(crate) c: [[f64; 6]; 6],
    /// 6x6 compliance matrix (inverse of `c`).
    pub(crate) s: [[f64; 6]; 6],
    pub(crate) e: f64,
    pub(crate) nu: f64,
    pub(crate) g: f64,
    pub(crate) zener: f64,
}

/// Parses an AMITEX `.std` result file: skips the 6-line header, then one row of
/// whitespace-separated floats per output line. Matches
/// `np.loadtxt(filename, skiprows=6)` in `runs2moduli.py`.
fn parse_std_file(path: &Path) -> anyhow::Result<Vec<Vec<f64>>> {
    let contents = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    contents
        .lines()
        .skip(6)
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            line.split_whitespace()
                .map(str::parse::<f64>)
                .collect::<Result<Vec<_>, _>>()
                .map_err(Into::into)
        })
        .collect()
}

/// Gauss-Jordan inverse of a fixed 6x6 matrix. Hand-rolled instead of pulling in
/// `ndarray-linalg`, which needs a linked LAPACK backend that differs between macOS and
/// Linux/WSL — not worth it for a single 6x6 inverse.
fn invert_6x6(m: &[[f64; 6]; 6]) -> anyhow::Result<[[f64; 6]; 6]> {
    let mut a = *m;
    let mut inv = [[0.0; 6]; 6];
    for i in 0..6 {
        inv[i][i] = 1.0;
    }

    for col in 0..6 {
        let pivot_row = (col..6)
            .max_by(|&r1, &r2| a[r1][col].abs().partial_cmp(&a[r2][col].abs()).unwrap())
            .unwrap();
        if a[pivot_row][col].abs() < 1e-14 {
            bail!("stiffness matrix is singular (or near-singular) at column {col}");
        }
        a.swap(col, pivot_row);
        inv.swap(col, pivot_row);

        let pivot = a[col][col];
        for j in 0..6 {
            a[col][j] /= pivot;
            inv[col][j] /= pivot;
        }

        for row in 0..6 {
            if row == col {
                continue;
            }
            let factor = a[row][col];
            for j in 0..6 {
                a[row][j] -= factor * a[col][j];
                inv[row][j] -= factor * inv[col][j];
            }
        }
    }

    Ok(inv)
}

/// Assembles the 6x6 stiffness matrix from the 6 load cases' `.std` files and derives
/// the homogenized elastic constants, ported from `runs2moduli.py`.
///
/// `std_paths[j]` is the result file for the load case with unit strain in Voigt
/// direction `j` (xx, yy, zz, xy, xz, yz) — column `j` of `C` is that case's stress
/// response.
pub(crate) fn compute_moduli(std_paths: &[std::path::PathBuf; 6]) -> anyhow::Result<ElasticModuli> {
    let mut c = [[0.0; 6]; 6];
    for (j, path) in std_paths.iter().enumerate() {
        let rows = parse_std_file(path)?;
        let last = rows
            .last()
            .ok_or_else(|| anyhow::anyhow!("{}: no data rows", path.display()))?;
        for i in 0..6 {
            c[i][j] = last[i + 1];
        }
    }

    let s = invert_6x6(&c)?;

    let e = 1.0 / s[0][0];
    let nu = -s[0][1] * e;
    let g = c[3][3];
    let zener = 2.0 * c[3][3] / (c[0][0] - c[0][1]);

    Ok(ElasticModuli { c, s, e, nu, g, zener })
}

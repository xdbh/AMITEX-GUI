/// Von Mises stress from the 6 Voigt stress components, ported from `stress6toVM.py`.
/// `sig` order: [sig_xx, sig_yy, sig_zz, sig_xy, sig_xz, sig_yz].
pub(crate) fn von_mises(sig: &[Vec<f64>; 6]) -> Vec<f64> {
    let n = sig[0].len();
    (0..n)
        .map(|k| {
            let s = |i: usize| sig[i][k];
            (((s(0) - s(1)).powi(2)
                + (s(1) - s(2)).powi(2)
                + (s(2) - s(0)).powi(2)
                + 6.0 * s(3).powi(2)
                + 6.0 * s(4).powi(2)
                + 6.0 * s(5).powi(2))
                / 2.0)
                .sqrt()
        })
        .collect()
}

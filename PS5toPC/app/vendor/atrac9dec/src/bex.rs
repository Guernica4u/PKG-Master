//! Band extension (BEX): rebuilds the high frequencies of low-bitrate streams.
//! Port of LibAtrac9 band_extension.c.

use crate::structures::{Block, Channel};
use crate::tables::{QUANT_UNIT_TO_COEFF_COUNT, QUANT_UNIT_TO_COEFF_INDEX, SPECTRUM_SCALE};

/// (band count, group B unit, group C unit), indexed by quantization unit count - 13.
pub const BEX_GROUP_INFO: [(usize, usize, usize); 8] =
    [(0, 16, 21), (1, 18, 22), (2, 20, 22), (3, 21, 22), (3, 21, 22), (4, 23, 24), (4, 23, 24), (5, 24, 24)];

pub const BEX_ENCODED_VALUE_COUNTS: [[usize; 6]; 5] =
    [[0, 0, 0, 4, 4, 2], [0, 0, 0, 0, 0, 0], [0, 0, 0, 2, 2, 1], [0, 0, 0, 2, 2, 2], [1, 1, 1, 0, 0, 0]];

/// [mode][bands][value index]
pub const BEX_DATA_LENGTHS: [[[u32; 4]; 6]; 5] = [
    [[0; 4], [0; 4], [0; 4], [5, 4, 3, 3], [4, 4, 3, 4], [4, 5, 0, 0]],
    [[0; 4]; 6],
    [[0; 4], [0; 4], [0; 4], [6, 6, 0, 0], [6, 6, 0, 0], [6, 0, 0, 0]],
    [[0; 4], [0; 4], [0; 4], [4, 4, 0, 0], [4, 4, 0, 0], [4, 4, 0, 0]],
    [[3, 0, 0, 0], [3, 0, 0, 0], [3, 0, 0, 0], [0; 4], [0; 4], [0; 4]],
];

pub fn apply_band_extension(block: &mut Block) {
    if !block.band_extension_enabled || !block.has_extension_data {
        return;
    }
    let units = block.quantization_unit_count;
    for ch in block.channels.iter_mut().take(block.channel_count) {
        apply_channel(ch, units);
    }
}

fn apply_channel(channel: &mut Channel, group_a_unit: usize) {
    let (band_count, group_b_unit, group_c_unit) = BEX_GROUP_INFO[group_a_unit - 13];
    let total_units = group_c_unit.max(22);
    let bex_quant_units = total_units - group_a_unit;
    let a = QUANT_UNIT_TO_COEFF_INDEX[group_a_unit];
    let b = QUANT_UNIT_TO_COEFF_INDEX[group_b_unit];
    let c = QUANT_UNIT_TO_COEFF_INDEX[group_c_unit];
    let total = QUANT_UNIT_TO_COEFF_INDEX[total_units];
    let v = channel.bex_values.map(|x| x.max(0) as usize);

    // Mirror the coded spectrum upwards into each group.
    for (start, end) in [(a, b), (b, c), (c, total)] {
        for i in 0..end - start {
            channel.spectra[start + i] = channel.spectra[start - i - 1];
        }
    }

    let mut scales = [0.0f64; 6];
    match channel.bex_mode {
        0 => {
            match band_count {
                3 => {
                    scales[0] = MODE0_BANDS3[0][v[0]];
                    scales[1] = MODE0_BANDS3[1][v[0]];
                    scales[2] = MODE0_BANDS3[2][v[1]];
                    scales[3] = MODE0_BANDS3[3][v[2]];
                    scales[4] = MODE0_BANDS3[4][v[3]];
                }
                4 => {
                    scales[0] = MODE0_BANDS4[0][v[0]];
                    scales[1] = MODE0_BANDS4[1][v[0]];
                    scales[2] = MODE0_BANDS4[2][v[1]];
                    scales[3] = MODE0_BANDS4[3][v[2]];
                    scales[4] = MODE0_BANDS4[4][v[3]];
                }
                5 => {
                    scales[0] = MODE0_BANDS5[0][v[0]];
                    scales[1] = MODE0_BANDS5[1][v[1]];
                    scales[2] = MODE0_BANDS5[2][v[1]];
                }
                _ => {}
            }
            scales[bex_quant_units - 1] = SPECTRUM_SCALE[channel.scale_factors[group_a_unit].clamp(0, 31) as usize];
            add_noise(
                channel,
                QUANT_UNIT_TO_COEFF_INDEX[total_units - 1],
                QUANT_UNIT_TO_COEFF_COUNT[total_units - 1] as usize,
            );
            scale_units(&mut channel.spectra, &scales, group_a_unit, total_units);
        }
        1 => {
            for i in group_a_unit..total_units {
                scales[i - group_a_unit] = SPECTRUM_SCALE[channel.scale_factors[i].clamp(0, 31) as usize];
            }
            add_noise(channel, a, total - a);
            scale_units(&mut channel.spectra, &scales, group_a_unit, total_units);
        }
        2 => {
            let (sa, sb) = (MODE2_SCALE[v[0]], MODE2_SCALE[v[1]]);
            channel.spectra[a..b].iter_mut().for_each(|s| *s *= sa);
            channel.spectra[b..c].iter_mut().for_each(|s| *s *= sb);
        }
        3 => {
            let rate = 2f64.powf(MODE3_RATE[v[1]]);
            let mut scale = MODE3_INITIAL[v[0]];
            for s in &mut channel.spectra[a..total] {
                scale *= rate;
                *s *= scale;
            }
        }
        4 => {
            let mult = MODE4_MULTIPLIER[v[0]];
            channel.spectra[a..b].iter_mut().for_each(|s| *s *= 0.7079468 * mult);
            channel.spectra[b..c].iter_mut().for_each(|s| *s *= 0.5011902 * mult);
            channel.spectra[c..total].iter_mut().for_each(|s| *s *= 0.3548279 * mult);
        }
        _ => {}
    }
}

fn scale_units(spectra: &mut [f64], scales: &[f64; 6], start_unit: usize, total_units: usize) {
    for i in start_unit..total_units {
        for k in QUANT_UNIT_TO_COEFF_INDEX[i]..QUANT_UNIT_TO_COEFF_INDEX[i + 1] {
            spectra[k] *= scales[i - start_unit];
        }
    }
}

fn add_noise(channel: &mut Channel, index: usize, count: usize) {
    let rng = channel.rng.get_or_insert_with(|| {
        let sf = &channel.scale_factors;
        let seed = (543 * (sf[8] + sf[12] + sf[15] + 1)) as u16;
        let start = 0x4D93i32.wrapping_mul((seed ^ (seed >> 14)) as i32);
        [3i32.wrapping_sub(start) as u16, 2i32.wrapping_sub(start) as u16, 1i32.wrapping_sub(start) as u16, 0i32.wrapping_sub(start) as u16]
    });
    for i in 0..count {
        let [a, b, c, d] = *rng;
        let t = d ^ (d << 5);
        let next = t ^ a ^ ((t ^ (a >> 5)) >> 4);
        *rng = [next, a, b, c];
        channel.spectra[index + i] = next as f64 / 65535.0 * 2.0 - 1.0;
    }
}

const MODE0_BANDS3: [[f64; 32]; 5] = [
    [
        0.000000e+0, 1.988220e-1, 2.514343e-1, 2.960510e-1, 3.263550e-1, 3.771362e-1, 3.786926e-1, 4.540405e-1,
        4.877625e-1, 5.262451e-1, 5.447083e-1, 5.737000e-1, 6.212158e-1, 6.222839e-1, 6.560974e-1, 6.896667e-1,
        7.555542e-1, 7.677917e-1, 7.918091e-1, 7.971497e-1, 8.188171e-1, 8.446045e-1, 9.790649e-1, 9.822083e-1,
        9.846191e-1, 9.859314e-1, 9.863586e-1, 9.863892e-1, 9.873352e-1, 9.881287e-1, 9.898682e-1, 9.913330e-1,
    ],
    [
        0.000000e+0, 9.982910e-1, 7.592773e-2, 7.179565e-1, 9.851379e-1, 5.340271e-1, 9.013672e-1, 6.349182e-1,
        7.226257e-1, 1.948547e-1, 7.628174e-1, 9.873657e-1, 8.112183e-1, 2.715454e-1, 9.734192e-1, 1.443787e-1,
        4.640198e-1, 3.249207e-1, 3.790894e-1, 8.276367e-2, 5.954590e-1, 2.864380e-1, 9.806824e-1, 7.929077e-1,
        6.292114e-1, 4.887085e-1, 2.905273e-1, 1.301880e-1, 3.140869e-1, 5.482483e-1, 4.210815e-1, 1.182861e-1,
    ],
    [
        0.000000e+0, 3.155518e-2, 8.581543e-2, 1.364746e-1, 1.858826e-1, 2.368469e-1, 2.888184e-1, 3.432617e-1,
        4.012451e-1, 4.623108e-1, 5.271301e-1, 5.954895e-1, 6.681213e-1, 7.448425e-1, 8.245239e-1, 9.097290e-1,
        0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0,
    ],
    [
        0.000000e+0, 4.418945e-2, 1.303711e-1, 2.273560e-1, 3.395996e-1, 4.735718e-1, 6.267090e-1, 8.003845e-1,
        0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0,
    ],
    [
        0.000000e+0, 2.804565e-2, 9.683228e-2, 1.849976e-1, 3.005981e-1, 4.470520e-1, 6.168518e-1, 8.007813e-1,
        0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0,
    ],
];

const MODE0_BANDS4: [[f64; 16]; 5] = [
    [
        0.000000e+0, 2.708740e-1, 3.479614e-1, 3.578186e-1, 5.083618e-1, 5.299072e-1, 5.819092e-1, 6.381836e-1,
        7.276917e-1, 7.595520e-1, 7.878723e-1, 9.707336e-1, 9.713135e-1, 9.736023e-1, 9.759827e-1, 9.832458e-1,
    ],
    [
        0.000000e+0, 2.330627e-1, 5.891418e-1, 7.170410e-1, 2.036438e-1, 1.613464e-1, 6.668701e-1, 9.481201e-1,
        9.769897e-1, 5.111694e-1, 3.522644e-1, 8.209534e-1, 2.933960e-1, 9.757690e-1, 5.289917e-1, 4.372253e-1,
    ],
    [
        0.000000e+0, 4.360962e-2, 1.056519e-1, 1.590576e-1, 2.078857e-1, 2.572937e-1, 3.082581e-1, 3.616028e-1,
        4.191589e-1, 4.792175e-1, 5.438538e-1, 6.125183e-1, 6.841125e-1, 7.589417e-1, 8.365173e-1, 9.148254e-1,
    ],
    [
        0.000000e+0, 4.074097e-2, 1.164551e-1, 2.077026e-1, 3.184509e-1, 4.532166e-1, 6.124268e-1, 7.932129e-1,
        0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0,
    ],
    [
        0.000000e+0, 8.880615e-3, 2.932739e-2, 5.593872e-2, 8.825684e-2, 1.259155e-1, 1.721497e-1, 2.270813e-1,
        2.901611e-1, 3.579712e-1, 4.334106e-1, 5.147095e-1, 6.023254e-1, 6.956177e-1, 7.952881e-1, 8.977356e-1,
    ],
];

const MODE0_BANDS5: [[f64; 32]; 3] = [
    [
        0.000000e+0, 7.379150e-2, 1.806335e-1, 2.687073e-1, 3.407898e-1, 4.047546e-1, 4.621887e-1, 5.168762e-1,
        5.703125e-1, 6.237488e-1, 6.763611e-1, 7.288208e-1, 7.808533e-1, 8.337708e-1, 8.874512e-1, 9.418030e-1,
        0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0,
    ],
    [
        0.000000e+0, 7.980347e-2, 1.615295e-1, 1.665649e-1, 1.822205e-1, 2.185669e-1, 2.292175e-1, 2.456665e-1,
        2.666321e-1, 3.306580e-1, 3.330688e-1, 3.765259e-1, 4.085083e-1, 4.400024e-1, 4.407654e-1, 4.817505e-1,
        4.924011e-1, 5.320740e-1, 5.893860e-1, 6.131287e-1, 6.212463e-1, 6.278076e-1, 6.308899e-1, 7.660828e-1,
        7.850647e-1, 7.910461e-1, 7.929382e-1, 8.038330e-1, 9.834900e-1, 9.846191e-1, 9.852295e-1, 9.862671e-1,
    ],
    [
        0.000000e+0, 6.084290e-1, 3.672791e-1, 3.151855e-1, 1.488953e-1, 2.571716e-1, 5.103455e-1, 3.311157e-1,
        5.426025e-2, 4.254456e-1, 7.998352e-1, 7.873230e-1, 5.418701e-1, 2.925110e-1, 8.468628e-2, 1.410522e-1,
        9.819641e-1, 9.609070e-1, 3.530884e-2, 9.729004e-2, 5.758362e-1, 9.941711e-1, 7.215576e-1, 7.183228e-1,
        2.028809e-1, 9.588623e-2, 2.032166e-1, 1.338806e-1, 5.003357e-1, 1.874390e-1, 9.804993e-1, 1.107788e-1,
    ],
];

const MODE2_SCALE: [f64; 64] = [
    4.272461e-4, 1.312256e-3, 2.441406e-3, 3.692627e-3, 4.913330e-3, 6.134033e-3, 7.507324e-3, 8.972168e-3,
    1.049805e-2, 1.223755e-2, 1.406860e-2, 1.599121e-2, 1.800537e-2, 2.026367e-2, 2.264404e-2, 2.517700e-2,
    2.792358e-2, 3.073120e-2, 3.344727e-2, 3.631592e-2, 3.952026e-2, 4.275513e-2, 4.608154e-2, 4.968262e-2,
    5.355835e-2, 5.783081e-2, 6.195068e-2, 6.677246e-2, 7.196045e-2, 7.745361e-2, 8.319092e-2, 8.993530e-2,
    9.759521e-2, 1.056213e-1, 1.138916e-1, 1.236267e-1, 1.348267e-1, 1.470337e-1, 1.603394e-1, 1.755676e-1,
    1.905823e-1, 2.071228e-1, 2.245178e-1, 2.444153e-1, 2.658997e-1, 2.897644e-1, 3.146057e-1, 3.450012e-1,
    3.766174e-1, 4.122620e-1, 4.505615e-1, 4.893799e-1, 5.305481e-1, 5.731201e-1, 6.157837e-1, 6.580811e-1,
    6.985168e-1, 7.435303e-1, 7.865906e-1, 8.302612e-1, 8.718567e-1, 9.125671e-1, 9.575806e-1, 9.996643e-1,
];

const MODE3_INITIAL: [f64; 16] = [
    3.491211e-1, 5.371094e-1, 6.782227e-1, 7.910156e-1, 9.057617e-1, 1.024902e+0, 1.156250e+0, 1.290527e+0,
    1.458984e+0, 1.664551e+0, 1.929688e+0, 2.278320e+0, 2.831543e+0, 3.659180e+0, 5.257813e+0, 8.373047e+0,
];

const MODE3_RATE: [f64; 16] = [
    -2.913818e-1, -2.541504e-1, -1.664429e-1, -1.476440e-1, -1.342163e-1, -1.220703e-1, -1.117554e-1, -1.026611e-1,
    -9.436035e-2, -8.483887e-2, -7.476807e-2, -6.304932e-2, -4.492188e-2, -2.447510e-2, 1.831055e-4, 4.174805e-2,
];

const MODE4_MULTIPLIER: [f64; 8] = [3.610229e-2, 1.260681e-1, 2.227478e-1, 3.338318e-1, 4.662170e-1, 6.221313e-1, 7.989197e-1, 9.939575e-1];

// Sonata
// Copyright (c) 2019 The Sonata Project Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::cmp::{max, min};
use std::{f32, f64};

use lazy_static::lazy_static;

use sonata_core::audio::{AudioBuffer, Signal, SignalSpec, Layout};
use sonata_core::errors::{Result, decode_error, unsupported_error};
use sonata_core::io::{BufStream, BitStream, BitStreamLtr, Bytestream, huffman::{H8, HuffmanTable}};

use super::huffman_tables::*;
use super::synthesis;

/// Bit-rate lookup table for MPEG version 1 layer 1.
static BIT_RATES_MPEG1_L1: [u32; 15] =
[
    0,
    32_000,  64_000,  96_000, 128_000, 160_000, 192_000, 224_000,
    256_000, 288_000, 320_000, 352_000, 384_000, 416_000, 448_000,
];

/// Bit-rate lookup table for MPEG version 1 layer 2.
static BIT_RATES_MPEG1_L2: [u32; 15] =
[
    0,
    32_000,  48_000,  56_000,  64_000,  80_000,  96_000, 112_000,
    128_000, 160_000, 192_000, 224_000, 256_000, 320_000, 384_000,
];

/// Bit-rate lookup table for MPEG version 1 layer 3.
static BIT_RATES_MPEG1_L3: [u32; 15] =
[
    0,
    32_000,  40_000,  48_000,  56_000,  64_000,  80_000,  96_000,
    112_000, 128_000, 160_000, 192_000, 224_000, 256_000, 320_000
];

/// Bit-rate lookup table for MPEG version 2 & 2.5 audio layer 1.
static BIT_RATES_MPEG2_L1: [u32; 15] =
[
    0,
    32_000,  48_000,  56_000,  64_000,  80_000,  96_000,  112_000,
    128_000, 144_000, 160_000, 176_000, 192_000, 224_000, 256_000,
];

/// Bit-rate lookup table for MPEG version 2 & 2.5 audio layers 2 & 3.
static BIT_RATES_MPEG2_L23: [u32; 15] =
[
    0,
    8_000,  16_000, 24_000, 32_000,  40_000,  48_000,  56_000,
    64_000, 80_000, 96_000, 112_000, 128_000, 144_000, 160_000,
];

/// Pairs of bit lengths for MPEG version 1 scale factors. For MPEG version 1, there are two
/// possible bit lengths for scale factors: slen1 and slen2. The first N of bands have scale factors
/// of bit length slen1, while the remaining bands have length slen2. The value of the switch point,
/// N, is determined by block type.
///
/// This table is indexed by scalefac_compress.
static SCALE_FACTOR_SLEN: [(u32, u32); 16] =
[
    (0, 0), (0, 1), (0, 2), (0, 3), (3, 0), (1, 1), (1, 2), (1, 3),
    (2, 1), (2, 2), (2, 3), (3, 1), (3, 2), (3, 3), (4, 2), (4, 3),
];

/// For MPEG version 2, each scale factor band has a different scale factor. The length in bits of
/// a scale factor (slen) can be one of 4 values. The values in this table indicate the number of
/// scale factors that have length slen[0..4]. Slen[0..4] is calculated from scalefac_compress.
///
/// This table is indexed by channel_mode, scalefac_compress, and block_type.
const SCALE_FACTOR_MPEG2_NSFB: [[[usize; 4]; 3]; 6] = [
    // Intensity stereo channel modes.
    [[ 7,  7, 7, 0], [12, 12, 12, 0], [ 6, 15, 12, 0]],
    [[ 6,  6, 6, 3], [12,  9,  9, 6], [ 6, 12,  9, 6]],
    [[ 8,  8, 5, 0], [15, 12,  9, 0], [ 6, 18,  9, 0]],
    // Other channel modes.
    [[ 6,  5, 5, 5], [ 9,  9,  9, 9], [ 6,  9,  9, 9]],
    [[ 6,  5, 7, 3], [ 9,  9, 12, 6], [ 6,  9, 12, 6]],
    [[11, 10, 0, 0], [18, 18,  0, 0], [15, 18,  0, 0]],
];

/// Startng indicies of each scale factor band at various sampling rates for long blocks.
const SCALE_FACTOR_LONG_BANDS: [[u32; 23]; 9] = [
    // 44.1 kHz, MPEG version 1, derived from ISO/IEC 11172-3 Table B.8
    [
        0, 4, 8, 12, 16, 20, 24, 30, 36, 44, 52, 62, 74, 90, 110, 134,
        162, 196, 238, 288, 342, 418, 576
    ],
    // 48 kHz
    [
        0, 4, 8, 12, 16, 20, 24, 30, 36, 42, 50, 60, 72, 88, 106, 128,
        156, 190, 230, 276, 330, 384, 576,
    ],
    // 32 kHz
    [
        0, 4, 8, 12, 16, 20, 24, 30, 36, 44, 54, 66, 82, 102, 126, 156,
        194, 240, 296, 364, 448, 550, 576,
    ],
    // 22.050 kHz, MPEG version 2, derived from ISO/IEC 13818-3 Table B.2
    [
        0, 4, 12, 18, 24, 30, 36, 44, 54, 66, 80, 96, 116, 140, 168, 200,
        238, 284, 336, 396, 464, 522, 576,
    ],
    // 24 kHz (330 should be 332?)
    [
        0, 6, 12, 18, 24, 30, 36, 44, 54, 66, 80, 96, 114, 136, 162, 194,
        232, 278, 332, 394, 464, 540, 576,
    ],
    // 16 kHz
    [
        0, 6, 12, 18, 24, 30, 36, 44, 54, 66, 80, 96, 116, 140, 168, 200,
        238, 284, 336, 396, 464, 522, 576,
    ],
    // 11.025 kHz, MPEG version 2.5
    [
        0, 6, 12, 18, 24, 30, 36, 44, 54, 66, 80, 96, 116, 140, 168, 200,
        238, 284, 336, 396, 464, 522, 576,
    ],
    // 12 kHz
    [
        0, 6, 12, 18, 24, 30, 36, 44, 54, 66, 80, 96, 116, 140, 168, 200,
        238, 284, 336, 396, 464, 522, 576,
    ],
    // 8 kHz
    [
        0, 12, 24, 36, 48, 60, 72, 88, 108, 132, 160, 192, 232, 280, 336, 400,
        476, 566, 568, 570, 572, 574, 576,
    ],
];

/// Starting indicies of each scale factor band at various sampling rates for short blocks. Each
/// value must be multiplied by 3 since there are three equal length windows per short scale factor
/// band.
const SCALE_FACTOR_SHORT_BANDS: [[u32; 14]; 9] = [
    // 44.1 kHz, MPEG version 1
    [ 0, 4, 8, 12, 16, 22, 30, 40,  52,  66,  84, 106, 136, 192 ],
    // 48 kHz
    [ 0, 4, 8, 12, 16, 22, 28, 38,  50,  64,  80, 100, 126, 192 ],
    // 32 kHz
    [ 0, 4, 8, 12, 16, 22, 30, 42,  58,  78, 104, 138, 180, 192 ],
    // 22.050 kHz, MPEG version 2
    [ 0, 4, 8, 12, 18, 24, 32, 42, 56, 74, 100, 132, 174, 192 ],
    // 24 kHz
    [ 0, 4, 8, 12, 18, 26, 36, 48, 62, 80, 104, 136, 180, 192 ],
    // 16 kHz
    [ 0, 4, 8, 12, 18, 26, 36, 48, 62, 80, 104, 134, 174, 192 ],
    // 11.025 kHz, MPEG version 2.5
    [ 0, 4, 8, 12, 18, 26, 36, 48, 62, 80, 104, 134, 174, 192 ],
    // 12 kHz
    [ 0, 4, 8, 12, 18, 26, 36, 48, 62, 80, 104, 134, 174, 192 ],
    // 8 kHz
    [ 0, 8, 16, 24, 36, 52, 72, 96, 124, 160, 162, 164, 166, 192 ],
];

lazy_static! {
    /// Lookup table for computing x(i) = s(i)^(4/3) where s(i) is a decoded Huffman sample. The
    /// value of s(i) is bound between 0..8207.
    static ref REQUANTIZE_POW43: [f32; 8207] = {
        // It is wasteful to initialize to 0.. however, Sonata policy is to limit unsafe code to
        // only sonata-core.
        //
        // TODO: Implement generic lookup table initialization in the core library.
        let mut pow43 = [0f32; 8207];
        for i in 0..8207 {
            pow43[i] = f32::powf(i as f32, 4.0 / 3.0);
        }
        pow43
    };
}

lazy_static! {
    /// Lookup table of cosine coefficients for a 12-point IMDCT.
    ///
    /// The table is derived from the expression:
    ///
    /// ```text
    /// cos12[i][k] = cos(PI/24.0 * (2*i + 1 + 12/2) * (2*k + 1))
    /// ```
    ///
    /// This table indexed by k and i.
    static ref IMDCT_COS_12: [[f32; 6]; 12] = {
        const PI_24: f64 = f64::consts::PI / 24.0;

        let mut cos12 = [[0f32; 6]; 12];

        for i in 0..12 {
            for k in 0..6 {
                cos12[i][k] = (PI_24 * ((2*i + (12 / 2) + 1) * (2*k + 1)) as f64).cos() as f32;
            }
        }

        cos12
    };
}

lazy_static! {
    /// Pair of lookup tables, CS and CA, for alias reduction.
    ///
    /// As per ISO/IEC 11172-3, CS and CA are calculated as follows:
    ///
    /// ```text
    /// cs[i] =  1.0 / sqrt(1.0 + c[i]^2)
    /// ca[i] = c[i] / sqrt(1.0 + c[i]^2)
    /// ```
    ///
    /// where:
    /// ```text
    /// c[i] = [ -0.6, -0.535, -0.33, -0.185, -0.095, -0.041, -0.0142, -0.0037 ]
    /// ```
    static ref ANTIALIAS_CS_CA: ([f32; 8], [f32; 8]) = {
        const C: [f64; 8] = [ -0.6, -0.535, -0.33, -0.185, -0.095, -0.041, -0.0142, -0.0037 ];

        let mut cs = [0f32; 8];
        let mut ca = [0f32; 8];

        for i in 0..8 {
            let sqrt = f64::sqrt(1.0 + (C[i] * C[i]));
            cs[i] = (1.0 / sqrt) as f32;
            ca[i] = (C[i] / sqrt) as f32;
        }

        (cs, ca)
    };
}

lazy_static! {
    /// (Left, right) channel coefficients for decoding intensity stereo in MPEG2 bitstreams.
    ///
    /// These coefficients are derived from section 2.4.3.2 of ISO/IEC 13818-3.
    ///
    /// As per the specification, for a given intensity position, is_pos (0 <= is_pos < 32), the
    /// channel coefficients, k_l and k_r, may be calculated as per the table below:
    ///
    /// ```text
    /// If...            | k_l                     | k_r
    /// -----------------+-------------------------+-------------------
    /// is_pos     == 0  | 1.0                     | 1.0
    /// is_pos & 1 == 1  | i0 ^ [(is_pos + 1) / 2] | 1.0
    /// is_pos & 1 == 0  | 1.0                     | i0 ^ (is_pos / 2)
    /// ```
    ///
    /// The value of i0 is dependant on the least significant bit of scalefac_compress.
    ///
    ///  ```text
    /// scalefac_compress & 1 | i0
    /// ----------------------+---------------------
    /// 0                     | 1 / sqrt(sqrt(2.0))
    /// 1                     | 1 / sqrt(2.0)
    /// ```
    ///
    /// The first dimension of this table is indexed by scalefac_compress & 1 to select i0. The
    /// second dimension is indexed by is_pos to obtain the channel coefficients. Note that
    /// is_pos == 7 is considered an invalid position, but IS included in the table.
    static ref INTENSITY_STEREO_RATIOS_MPEG2: [[(f32, f32); 32]; 2] = {
        let is_scale: [f64; 2] = [
            1.0 / f64::sqrt(f64::sqrt(2.0)),
            1.0 / f64::sqrt(2.0),
        ];

        let mut i = 0;
        let mut ratios = [[(0.0, 0.0); 32]; 2];

        for is_pos in 0..32 {
            if is_pos & 1 != 0 {
                ratios[0][i] = (f64::powi(is_scale[0], (is_pos + 1) >> 1) as f32, 1.0);
                ratios[1][i] = (f64::powi(is_scale[1], (is_pos + 1) >> 1) as f32, 1.0);
            }
            else {
                ratios[0][i] = (1.0, f64::powi(is_scale[0], is_pos >> 1) as f32);
                ratios[1][i] = (1.0, f64::powi(is_scale[1], is_pos >> 1) as f32);
            }
            i += 1;
        }

        ratios
    };
}

lazy_static! {
    /// (Left, right) channel coeffcients for decoding intensity stereo in MPEG1 bitstreams.
    ///
    /// These coefficients are derived from section 2.4.3.4.9.3 of ISO/IEC 11172-3.
    ///
    /// As per the specification, for a given intensity position, is_pos (0 <= is_pos < 7), a ratio,
    /// is_ratio, is calculated as follows:
    ///
    /// ```text
    /// is_ratio = tan(is_pos * PI/12)
    /// ```
    ///
    /// Then, the channel coefficients, k_l and k_r, are calculated as follows:
    ///
    /// ```text
    /// k_l = is_ratio / (1 + is_ratio)
    /// k_r =        1 / (1 + is_ratio)
    /// ```
    ///
    /// This table is indexed by is_pos. Note that is_pos == 7 is invalid and is NOT included in the
    /// table.
    static ref INTENSITY_STEREO_RATIOS: [(f32, f32); 7] = {
        const PI_12: f64 = f64::consts::PI / 12.0;

        let mut ratios = [(0.0, 0.0); 7];

        for is_pos in 0..6 {
            let ratio = (PI_12 * is_pos as f64).tan();
            ratios[is_pos] = ((ratio / (1.0 + ratio)) as f32, 1.0 / (1.0 + ratio) as f32);
        }

        ratios[6] = (1.0, 0.0);

        ratios
    };
}

lazy_static! {
    /// Post-IMDCT window coefficients for each block type: Long, Start, End, Short, in that order.
    ///
    /// For long blocks:
    ///
    /// ```text
    /// W[ 0..36] = sin(PI/36.0 * (i + 0.5))
    /// ```
    ///
    /// For start blocks:
    ///
    /// ```text
    /// W[ 0..18] = sin(PI/36.0 * (i + 0.5))
    /// W[18..24] = 1.0
    /// W[24..30] = sin(PI/12.0 * ((i - 18) - 0.5))
    /// W[30..36] = 0.0
    /// ```
    ///
    /// For end blocks:
    ///
    /// ```text
    /// W[ 0..6 ] = 0.0
    /// W[ 6..12] = sin(PI/12.0 * ((i - 6) + 0.5))
    /// W[12..18] = 1.0
    /// W[18..36] = sin(PI/36.0 * (i + 0.5))
    /// ```
    ///
    /// For short blocks (to be applied to each 12 sample window):
    ///
    /// ```text
    /// W[ 0..12] = sin(PI/12.0 * (i + 0.5))
    /// W[12..24] = W[0..12]
    /// W[24..36] = W[0..12]
    /// ```
    static ref IMDCT_WINDOWS: [[f32; 36]; 4] = {
        const PI_36: f64 = f64::consts::PI / 36.0;
        const PI_12: f64 = f64::consts::PI / 12.0;

        let mut windows = [[0f32; 36]; 4];

        // Window for Long blocks.
        for i in 0..36 {
            windows[0][i] = (PI_36 * (i as f64 + 0.5)).sin() as f32;
        }

        // Window for Start blocks (indicies 30..36 implictly 0.0).
        for i in 0..18 {
            windows[1][i] = (PI_36 * (i as f64 + 0.5)).sin() as f32;
        }
        for i in 18..24 {
            windows[1][i] = 1.0;
        }
        for i in 24..30 {
            windows[1][i] = (PI_12 * ((i - 18) as f64 + 0.5)).sin() as f32;
        }

        // Window for End blocks (indicies 0..6 implicitly 0.0).
        for i in 6..12 {
            windows[2][i] = (PI_12 * ((i - 6) as f64 + 0.5)).sin() as f32;
        }
        for i in 12..18 {
            windows[2][i] = 1.0;
        }
        for i in 18..36 {
            windows[2][i] = (PI_36 * (i as f64 + 0.5)).sin() as f32;
        }

        // Window for Short blocks.
        for i in 0..12 {
            // Repeat the window 3 times over.
            windows[3][0*12 + i] = (PI_12 * (i as f64 + 0.5)).sin() as f32;
            windows[3][1*12 + i] = windows[3][i];
            windows[3][2*12 + i] = windows[3][i];
        }

        windows
   };
}

struct MpegHuffmanTable {
    /// The Huffman decode table.
    huff_table: &'static HuffmanTable<H8>,
    /// Number of extra bits to read if the decoded Huffman value is saturated.
    linbits: u32,
}

const HUFFMAN_TABLES: [MpegHuffmanTable; 32] = [
    // Table 0
    MpegHuffmanTable { huff_table: &HUFFMAN_TABLE_0,  linbits:  0 },
    // Table 1
    MpegHuffmanTable { huff_table: &HUFFMAN_TABLE_1,  linbits:  0 },
    // Table 2
    MpegHuffmanTable { huff_table: &HUFFMAN_TABLE_2,  linbits:  0 },
    // Table 3
    MpegHuffmanTable { huff_table: &HUFFMAN_TABLE_3,  linbits:  0 },
    // Table 4 (not used)
    MpegHuffmanTable { huff_table: &HUFFMAN_TABLE_0,  linbits:  0 },
    // Table 5
    MpegHuffmanTable { huff_table: &HUFFMAN_TABLE_5,  linbits:  0 },
    // Table 6
    MpegHuffmanTable { huff_table: &HUFFMAN_TABLE_6,  linbits:  0 },
    // Table 7
    MpegHuffmanTable { huff_table: &HUFFMAN_TABLE_7,  linbits:  0 },
    // Table 8
    MpegHuffmanTable { huff_table: &HUFFMAN_TABLE_8,  linbits:  0 },
    // Table 9
    MpegHuffmanTable { huff_table: &HUFFMAN_TABLE_9,  linbits:  0 },
    // Table 10
    MpegHuffmanTable { huff_table: &HUFFMAN_TABLE_10, linbits:  0 },
    // Table 11
    MpegHuffmanTable { huff_table: &HUFFMAN_TABLE_11, linbits:  0 },
    // Table 12
    MpegHuffmanTable { huff_table: &HUFFMAN_TABLE_12, linbits:  0 },
    // Table 13
    MpegHuffmanTable { huff_table: &HUFFMAN_TABLE_13, linbits:  0 },
    // Table 14 (not used)
    MpegHuffmanTable { huff_table: &HUFFMAN_TABLE_0,  linbits:  0 },
    // Table 15
    MpegHuffmanTable { huff_table: &HUFFMAN_TABLE_15, linbits:  0 },
    // Table 16
    MpegHuffmanTable { huff_table: &HUFFMAN_TABLE_16, linbits:  1 },
    // Table 17
    MpegHuffmanTable { huff_table: &HUFFMAN_TABLE_16, linbits:  2 },
    // Table 18
    MpegHuffmanTable { huff_table: &HUFFMAN_TABLE_16, linbits:  3 },
    // Table 19
    MpegHuffmanTable { huff_table: &HUFFMAN_TABLE_16, linbits:  4 },
    // Table 20
    MpegHuffmanTable { huff_table: &HUFFMAN_TABLE_16, linbits:  6 },
    // Table 21
    MpegHuffmanTable { huff_table: &HUFFMAN_TABLE_16, linbits:  8 },
    // Table 22
    MpegHuffmanTable { huff_table: &HUFFMAN_TABLE_16, linbits: 10 },
    // Table 23
    MpegHuffmanTable { huff_table: &HUFFMAN_TABLE_16, linbits: 13 },
    // Table 24
    MpegHuffmanTable { huff_table: &HUFFMAN_TABLE_24, linbits:  4 },
    // Table 25
    MpegHuffmanTable { huff_table: &HUFFMAN_TABLE_24, linbits:  5 },
    // Table 26
    MpegHuffmanTable { huff_table: &HUFFMAN_TABLE_24, linbits:  6 },
    // Table 27
    MpegHuffmanTable { huff_table: &HUFFMAN_TABLE_24, linbits:  7 },
    // Table 28
    MpegHuffmanTable { huff_table: &HUFFMAN_TABLE_24, linbits:  8 },
    // Table 29
    MpegHuffmanTable { huff_table: &HUFFMAN_TABLE_24, linbits:  9 },
    // Table 30
    MpegHuffmanTable { huff_table: &HUFFMAN_TABLE_24, linbits: 11 },
    // Table 31
    MpegHuffmanTable { huff_table: &HUFFMAN_TABLE_24, linbits: 13 },
];

/// The MPEG audio version.
#[derive(Copy,Clone,Debug,PartialEq)]
enum MpegVersion {
    /// Version 2.5
    Mpeg2p5,
    /// Version 2
    Mpeg2,
    /// Version 1
    Mpeg1,
}

/// The MPEG audio layer.
#[derive(Copy,Clone,Debug,PartialEq)]
enum MpegLayer {
    /// Layer 1
    Layer1,
    /// Layer 2
    Layer2,
    /// Layer 3
    Layer3,
}

/// For Joint Stereo mode, the mode extension describes the features and parameters of the Joint
/// Stereo encoding.
#[derive(Copy,Clone,Debug,PartialEq)]
enum Mode {
    /// Joint Stereo in layer 3 may use both Mid-Side and Intensity encoding.
    Layer3 { mid_side: bool, intensity: bool },
    /// Joint Stereo in layers 1 and 2 may only use Intensity encoding on a set of bands. The range
    /// of bands is [bound..32].
    Intensity { bound: u32 },
}

/// The channel mode.
#[derive(Copy,Clone,Debug,PartialEq)]
enum Channels {
    /// Single mono audio channel.
    Mono,
    /// Dual mono audio channels.
    DualMono,
    /// Stereo channels.
    Stereo,
    /// Joint Stereo encoded channels (decodes to Stereo).
    JointStereo(Mode),
}

impl Channels {
    /// Gets the number of channels.
    #[inline(always)]
    fn count(&self) -> usize {
        match *self {
            Channels::Mono => 1,
            _              => 2,
        }
    }
}

/// The emphasis applied during encoding.
#[derive(Copy,Clone,Debug,PartialEq)]
enum Emphasis {
    /// No emphasis
    None,
    /// 50/15us
    Fifty15,
    /// CCIT J.17
    CcitJ17,
}

/// A MPEG 1, 2, or 2.5 audio frame header.
#[derive(Debug)]
pub struct FrameHeader {
    version: MpegVersion,
    layer: MpegLayer,
    bitrate: u32,
    sample_rate: u32,
    sample_rate_idx: usize,
    channels: Channels,
    emphasis: Emphasis,
    is_copyrighted: bool,
    is_original: bool,
    has_padding: bool,
    crc: Option<u16>,
    frame_size: usize,
}

impl FrameHeader {
    /// Returns true if this a MPEG1 frame, false otherwise.
    #[inline(always)]
    pub fn is_mpeg1(&self) -> bool {
        self.version == MpegVersion::Mpeg1
    }

    /// Returns true if this a MPEG2.5 frame, false otherwise.
    #[inline(always)]
    pub fn is_mpeg2p5(&self) -> bool {
        self.version == MpegVersion::Mpeg2p5
    }

    /// Returns true if this is a Layer 1 frame, false otherwise.
    #[inline(always)]
    pub fn is_layer1(&self) -> bool {
        self.layer == MpegLayer::Layer1
    }

    /// Returns true if this is a Layer 2 frame, false otherwise.
    #[inline(always)]
    pub fn is_layer2(&self) -> bool {
        self.layer == MpegLayer::Layer2
    }

    /// Returns true if this is a Layer 3 frame, false otherwise.
    #[inline(always)]
    pub fn is_layer3(&self) -> bool {
        self.layer == MpegLayer::Layer3
    }

    /// Returns a signal specification for the frame.
    pub fn spec(&self) -> SignalSpec {
        let layout = match self.n_channels() {
            1 => Layout::Mono,
            2 => Layout::Stereo,
            _ => unreachable!(),
        };

        SignalSpec::new_with_layout(self.sample_rate, layout)
    }

    /// Returns the number of granules in the frame.
    #[inline(always)]
    fn n_granules(&self) -> usize {
        match self.version {
            MpegVersion::Mpeg1 => 2,
            _                  => 1,
        }
    }

    /// Returns the number of channels per granule.
    #[inline(always)]
    fn n_channels(&self) -> usize {
        self.channels.count()
    }

    /// Returns true if Intensity Stereo encoding is used, false otherwise.
    #[inline(always)]
    fn is_intensity_stereo(&self) -> bool {
        match self.channels {
            Channels::JointStereo(Mode::Intensity { .. }) => true,
            Channels::JointStereo(Mode::Layer3 { intensity, .. }) => intensity,
            _ => false,
        }
    }
}

/// `FrameData` contains the side_info and main_data portions of a MPEG audio frame. Once read from
/// the bitstream, `FrameData` is immutable for the remainder of the decoding process.
#[derive(Default)]
struct FrameData {
    /// The byte offset into the bit resevoir indicating the location of the first bit of main_data.
    /// If 0, main_data begins after the side_info of this frame.
    main_data_begin: u16,
    /// Scale factor selector information, per channel. Each channel has 4 groups of bands that may
    /// be scaled in each granule. Scale factors may optionally be used by both granules to save
    /// bits. Bands that share scale factors for both granules are indicated by a true. Otherwise,
    /// each granule must store its own set of scale factors.
    ///
    /// Mapping of array indicies to bands [0-5, 6-10, 11-15, 16-20].
    scfsi: [[bool; 4]; 2],
    /// The granules.
    granules: [Granule; 2],
}

impl FrameData {
    /// Get a mutable slice to the granule(s) in side_info. For MPEG1, a slice of 2 granules are
    /// returned. For MPEG2/2.5, a single granule slice is returned.
    #[inline(always)]
    fn granules_mut(&mut self, version: MpegVersion) -> &mut [Granule] {
        match version {
            MpegVersion::Mpeg1 => &mut self.granules[..2],
            _                  => &mut self.granules[..1],
        }
    }
}

#[derive(Debug,PartialEq)]
enum BlockType {
    // Default case when window switching is off. Also the normal case when window switching is
    // on. Granule contains one long block.
    Long,
    Start,
    Short { is_mixed: bool },
    End
}

#[derive(Default)]
struct Granule {
    /// Channels in the granule.
    channels: [GranuleChannel; 2],
}

struct GranuleChannel {
    /// Total number of bits used for scale factors (part2), and Huffman encoded data (part3).
    part2_3_length: u16,
    /// HALF the number of samples in the big_values (sum of samples in region[0..3]) partition.
    big_values: u16,
    /// Quantization step size.
    global_gain: u8,
    /// Depending on the MPEG version, `scalefac_compress` determines how many bits are allocated
    /// per scale factor.
    ///
    /// - For MPEG1 bitstreams, `scalefac_compress` is a 4-bit index into SCALE_FACTOR_SLEN[0..16]
    /// to obtain a number of bits per scale factor pair.
    ///
    /// - For MPEG2/2.5 bitstreams, `scalefac_compress` is a 9-bit value that decodes into
    /// slen[0..3] (referred to as slen1-4 in the standard) for the number of bits per scale factor,
    /// and depending on which range the value falls into, for which bands.
    scalefac_compress: u16,
    /// Indicates the block type (type of window) for the channel in the granule.
    block_type: BlockType,
    /// Gain factors for region[0..3] in big_values. Each gain factor has a maximum value of 7
    /// (3 bits).
    subblock_gain: [u8; 3],
    /// The Huffman table to use for decoding region[0..3] in big_values.
    table_select: [u8; 3],
    /// The index of the first sample in region1 of big_values.
    region1_start: u32,
    /// The index of the first sample in region2 of big_values.
    region2_start: u32,
    /// Indicates if the pre-emphasis amount for each scale factor band should be added on to each 
    /// scale factor before requantization.
    preflag: bool,
    /// A 0.5x (false) or 1x (true) multiplier for scale factors.
    scalefac_scale: bool,
    /// Use Huffman table A (false) or B (true), for decoding the count1 partition.
    count1table_select: bool,
    /// Long (scalefac_l) and short (scalefac_s) window scale factor bands. Must be interpreted
    /// based on the block type of the granule.
    ///
    /// For `block_type == BlockType::Short { is_mixed: false }`:
    ///   - scalefac_s[0..36] -> scalefacs[0..36]
    ///
    /// For `block_type == BlockType::Short { is_mixed: true }`:
    ///   - scalefac_l[0..8]  -> scalefacs[0..8]
    ///   - scalefac_s[0..27] -> scalefacs[8..35]
    ///
    /// For `block_type != BlockType::Short { .. }`:
    ///   - scalefac_l[0..21] -> scalefacs[0..21]
    ///
    /// Note: The standard doesn't explicitly call it out, but for Short blocks, there are three
    ///       additional scale factors, scalefacs[36..39], that are always 0 and are not transmitted
    ///       in the bitstream.
    ///
    /// For MPEG1 and MPEG 2 without intensity stereo coding a scale factor will not exceed 4 bits
    /// in length (maximum value 15). For MPEG2 with intensity stereo, a scale factor will not
    /// exceed 5 bits (maximum value 31).
    scalefacs: [u8; 39],
    /// The starting sample of the rzero partition.
    rzero: usize,
}

impl Default for GranuleChannel {
    fn default() -> Self {
        GranuleChannel {
            part2_3_length: 0,
            big_values: 0,
            global_gain: 0,
            scalefac_compress: 0,
            block_type: BlockType::Long,
            subblock_gain: [0; 3],
            table_select: [0; 3],
            region1_start: 0,
            region2_start: 0,
            preflag: false,
            scalefac_scale: false,
            count1table_select: false,
            scalefacs: [0; 39],
            rzero: 0,
        }
    }
}

/// Synchronize the provided reader to the end of the frame header, and return the frame header as
/// as `u32`.
fn sync_frame<B: Bytestream>(reader: &mut B) -> Result<u32> {
    let mut sync = 0u32;

    // Synchronize stream to the next frame using the sync word. The MP3 frame header always starts
    // at a byte boundary with 0xffe (11 consecutive 1 bits.) if supporting up to MPEG version 2.5.
    while (sync & 0xffe00000) != 0xffe00000 {
        sync = sync.wrapping_shl(8) | reader.read_u8()? as u32;
    }

    Ok(sync)
}

/// Reads a MPEG audio frame header from the stream and return it or an error.
pub fn read_frame_header<B: Bytestream>(reader: &mut B) -> Result<FrameHeader> {
    // Synchronize and read the frame header.
    let header = sync_frame(reader)?;

    // The MP3 header is structured as follows:
    //
    // 0b1111_1111 0b111v_vlly 0brrrr_hhpx 0bmmmm_coee
    // where:
    //     vv   = version, ll = layer      , y = crc
    //     rrrr = bitrate, hh = sample rate, p = padding , x  = private bit
    //     mmmm = mode   , c  = copyright  , o = original, ee = emphasis

    let version = match (header & 0x18_0000) >> 19 {
        0b00 => MpegVersion::Mpeg2p5,
        0b10 => MpegVersion::Mpeg2,
        0b11 => MpegVersion::Mpeg1,
        _    => return decode_error("Invalid MPEG version."),
    };

    let layer = match (header & 0x6_0000) >> 17 {
        0b01 => MpegLayer::Layer3,
        0b10 => MpegLayer::Layer2,
        0b11 => MpegLayer::Layer1,
        _    => return decode_error("Invalid MPEG layer."),
    };

    let bitrate = match ((header & 0xf000) >> 12, version, layer) {
        // "Free" bit-rate. Note, this is NOT variable bit-rate and is not a mandatory feature of
        // MP3 decoders.
        (0b0000, _, _) => return unsupported_error("Free bit-rate is not supported."),
        // Invalid bit-rate.
        (0b1111, _, _) => return decode_error("Invalid bit-rate."),
        // MPEG 1 bit-rates.
        (i, MpegVersion::Mpeg1, MpegLayer::Layer1) => BIT_RATES_MPEG1_L1[i as usize],
        (i, MpegVersion::Mpeg1, MpegLayer::Layer2) => BIT_RATES_MPEG1_L2[i as usize],
        (i, MpegVersion::Mpeg1, MpegLayer::Layer3) => BIT_RATES_MPEG1_L3[i as usize],
        // MPEG 2 bit-rates.
        (i,                  _, MpegLayer::Layer1) => BIT_RATES_MPEG2_L1[i as usize],
        (i,                  _,                 _) => BIT_RATES_MPEG2_L23[i as usize],
    };

    let (sample_rate, sample_rate_idx) = match ((header & 0xc00) >> 10, version) {
        (0b00, MpegVersion::Mpeg1)   => (44_100, 0),
        (0b01, MpegVersion::Mpeg1)   => (48_000, 1),
        (0b10, MpegVersion::Mpeg1)   => (32_000, 2),
        (0b00, MpegVersion::Mpeg2)   => (22_050, 3),
        (0b01, MpegVersion::Mpeg2)   => (24_000, 4),
        (0b10, MpegVersion::Mpeg2)   => (16_000, 5),
        (0b00, MpegVersion::Mpeg2p5) => (11_025, 6),
        (0b01, MpegVersion::Mpeg2p5) => (12_000, 7),
        (0b10, MpegVersion::Mpeg2p5) => ( 8_000, 8),
        _                            => return decode_error("Invalid sample rate."),
    };

    let channels = match ((header & 0xc0) >> 6, layer) {
        // Stereo, for layers 1, 2, and 3.
        (0b00,                 _) => Channels::Stereo,
        // Dual mono, for layers 1, 2, and 3.
        (0b10,                 _) => Channels::DualMono,
        // Mono, for layers 1, 2, and 3.
        (0b11,                 _) => Channels::Mono,
        // Joint stereo mode for layer 3 supports a combination of Mid-Side and Intensity Stereo
        // depending on the mode extension bits.
        (0b01, MpegLayer::Layer3) => Channels::JointStereo(Mode::Layer3 {
            mid_side:  header & 0x20 != 0x0,
            intensity: header & 0x10 != 0x0,
        }),
        // Joint stereo mode for layers 1 and 2 only supports Intensity Stereo. The mode extension
        // bits indicate for which sub-bands intensity stereo coding is applied.
        (0b01,                 _) => Channels::JointStereo(Mode::Intensity {
            bound: (1 + (header & 0x30) >> 4) << 2,
        }),
        _                         => unreachable!(),
    };

    // Some layer 2 channel and bit-rate combinations are not allowed. Check that the frame does not
    // use them.
    if layer == MpegLayer::Layer2 {
        if channels == Channels::Mono {
            if bitrate == 224_000
                || bitrate == 256_000
                || bitrate == 320_000
                || bitrate == 384_000
            {
                return decode_error("Invalid Layer 2 bitrate for mono channel mode.");
            }
        }
        else {
            if bitrate == 32_000
                || bitrate == 48_000
                || bitrate == 56_000
                || bitrate == 80_000
            {
                return decode_error("Invalid Layer 2 bitrate for non-mono channel mode.");
            }
        }
    }

    let emphasis = match header & 0x3 {
        0b00 => Emphasis::None,
        0b01 => Emphasis::Fifty15,
        0b11 => Emphasis::CcitJ17,
        _    => return decode_error("Invalid emphasis."),
    };

    let is_copyrighted = header & 0x8 != 0x0;
    let is_original = header & 0x4 != 0x0;
    let has_padding = header & 0x200 != 0;

    let crc = if header & 0x1_0000 == 0 {
        Some(reader.read_be_u16()?)
    }
    else {
        None
    };

    // Calculate the size of the frame excluding this header.
    let frame_size =
        (if version == MpegVersion::Mpeg1 { 144 } else { 72 } * bitrate / sample_rate) as usize
        + if has_padding { 1 } else { 0 }
        - if crc.is_some() { 2 } else { 0 }
        - 4;

    Ok(FrameHeader{
        version,
        layer,
        bitrate,
        sample_rate,
        sample_rate_idx,
        channels,
        emphasis,
        is_copyrighted,
        is_original,
        has_padding,
        crc,
        frame_size,
    })
}

/// Reads the side_info for a single channel in a granule from a `BitStream`.
fn read_granule_channel_side_info_l3<B: BitStream>(
    bs: &mut B,
    channel: &mut GranuleChannel,
    header: &FrameHeader,
) -> Result<()> {

    channel.part2_3_length = bs.read_bits_leq32(12)? as u16;
    channel.big_values = bs.read_bits_leq32(9)? as u16;

    // The maximum number of samples in a granule is 576. One big_value decodes to 2 samples,
    // therefore there can be no more than 288 (576/2) big_values.
    if channel.big_values > 288 {
        return decode_error("Granule big_values > 288.");
    }

    channel.global_gain = bs.read_bits_leq32(8)? as u8;

    channel.scalefac_compress = if header.is_mpeg1() {
        bs.read_bits_leq32(4)
    }
    else {
        bs.read_bits_leq32(9)
    }? as u16;

    let window_switching = bs.read_bit()?;

    if window_switching {
        let block_type_enc = bs.read_bits_leq32(2)?;

        let is_mixed = bs.read_bit()?;

        channel.block_type = match block_type_enc {
            // Long block types are not allowed with window switching.
            0b00 => return decode_error("Invalid block_type."),
            0b01 => BlockType::Start,
            0b10 => BlockType::Short { is_mixed },
            0b11 => BlockType::End,
            _ => unreachable!(),
        };

        // When window switching is used, there are only two regions, therefore there are only
        // two table selectors.
        for i in 0..2 {
            channel.table_select[i] = bs.read_bits_leq32(5)? as u8;
        }

        for i in 0..3 {
            channel.subblock_gain[i] = bs.read_bits_leq32(3)? as u8;
        }

        // When using window switching, the boundaries of region[0..3] are set implicitly according
        // to the MPEG version and block type. Below, the boundaries to set as per the applicable
        // standard.
        //
        // If MPEG version 2.5 specifically...
        if header.is_mpeg2p5() {
            // For MPEG2.5, the number of scale-factor bands in region0 depends on the block type.
            // The standard indicates these values as 1 less than the actual value, therefore 1 is
            // added here to both values.
            let region0_count = match channel.block_type {
                BlockType::Short { is_mixed: false } => 5 + 1,
                _                                    => 7 + 1,
            };

            channel.region1_start = SCALE_FACTOR_LONG_BANDS[header.sample_rate_idx][region0_count];
        }
        // If MPEG version 1, OR the block type is Short...
        else if header.is_mpeg1() || block_type_enc == 0b11 {
            // For MPEG1 with LONG blocks, the first 8 LONG scale-factor bands are used for region0.
            // These bands are always [4, 4, 4, 4, 4, 4, 6, 6, ...] regardless of sample rate. These
            // bands sum to 36 samples.
            //
            // For MPEG1 with SHORT blocks, the first 9 SHORT scale-factor bands are used for
            // region0. These band are always [4, 4, 4, 4, 4, 4, 4, 4, 4, ...] regardless of sample
            // rate. These bands also sum to 36 samples.
            //
            // Finally, for MPEG2 with SHORT blocks, the first 9 short scale-factor bands are used
            // for region0. These bands are also always  [4, 4, 4, 4, 4, 4, 4, 4, 4, ...] regardless
            // of sample and thus sum to 36 samples.
            //
            // In all cases, the region0_count is 36.
            channel.region1_start = 36;
        }
        // If MPEG version 2 AND the block type is not Short...
        else {
            // For MPEG2 and LONG blocks, the first 8 LONG scale-factor bands are used for region0.
            // These bands are always [6, 6, 6, 6, 6, 6, 8, 10, ...] regardless of sample rate.
            // These bands sum to 54.
            channel.region1_start = 54;
        }

        // The second region, region1, spans the remaining samples. Therefore the third region,
        // region2, isn't used.
        channel.region2_start = 576;
    }
    else {
        // If window switching is not used, the block type is always Long.
        channel.block_type = BlockType::Long;

        for i in 0..3 {
            channel.table_select[i] = bs.read_bits_leq32(5)? as u8;
        }

        // When window switching is not used, only LONG scale-factor bands are used for each region.
        // The number of bands in region0 and region1 are defined in side_info. The stored value is
        // 1 less than the actual value.
        let region0_count   = bs.read_bits_leq32(4)? as usize + 1;
        let region0_1_count = bs.read_bits_leq32(3)? as usize + region0_count + 1;

        channel.region1_start = SCALE_FACTOR_LONG_BANDS[header.sample_rate_idx][region0_count];

        // The count in region0_1_count may exceed the last band (22) in the LONG bands table.
        // Protect against this.
        channel.region2_start = match region0_1_count {
            0..=22 => SCALE_FACTOR_LONG_BANDS[header.sample_rate_idx][region0_1_count],
            _      => 576,
        };
    }

    channel.preflag = if header.is_mpeg1() {
        bs.read_bit()?
    }
    else {
        // Pre-flag is determined implicitly for MPEG2: ISO/IEC 13818-3 section 2.4.3.4.
        channel.scalefac_compress >= 500
    };

    channel.scalefac_scale = bs.read_bit()?;
    channel.count1table_select = bs.read_bit()?;

    Ok(())
}

/// Reads the side_info for all channels in a granule from a `BitStream`.
fn read_granule_side_info_l3<B: BitStream>(
    bs: &mut B,
    granule: &mut Granule,
    header: &FrameHeader,
) -> Result<()> {
    // Read the side_info for each channel in the granule.
    for channel in &mut granule.channels[..header.channels.count()] {
        read_granule_channel_side_info_l3(bs, channel, header)?;
    }
    Ok(())
}

/// Reads the side_info of a MPEG audio frame from a `BitStream` into `FrameData`.
fn l3_read_side_info<B: Bytestream>(
    reader: &mut B,
    header: &FrameHeader,
    frame_data: &mut FrameData
) -> Result<usize> {

    let mut bs = BitStreamLtr::new(reader);

    // For MPEG version 1...
    let side_info_len = if header.is_mpeg1() {
        // First 9 bits is main_data_begin.
        frame_data.main_data_begin = bs.read_bits_leq32(9)? as u16;

        // Next 3 (>1 channel) or 5 (1 channel) bits are private and should be ignored.
        match header.channels {
            Channels::Mono => bs.ignore_bits(5)?,
            _              => bs.ignore_bits(3)?,
        };

        // Next four (or 8, if more than one channel) are the SCFSI bits.
        for scfsi in &mut frame_data.scfsi[..header.n_channels()] {
            for i in 0..4 {
                scfsi[i] = bs.read_bit()?;
            }
        }

        // The size of the side_info, fixed for layer 3.
        match header.channels {
            Channels::Mono => 17,
            _              => 32,
        }
    }
    // For MPEG version 2...
    else {
        // First 8 bits is main_data_begin.
        frame_data.main_data_begin = bs.read_bits_leq32(8)? as u16;

        // Next 1 (1 channel) or 2 (>1 channel) bits are private and should be ignored.
        match header.channels {
            Channels::Mono => bs.ignore_bits(1)?,
            _              => bs.ignore_bits(2)?,
        };

        // The size of the side_info, fixed for layer 3.
        match header.channels {
            Channels::Mono =>  9,
            _              => 17,
        }
    };

    // Read the side_info for each granule.
    for granule in frame_data.granules_mut(header.version) {
        read_granule_side_info_l3(&mut bs, granule, header)?;
    }

    Ok(side_info_len)
}

/// Reads the scale factors for a single channel in a granule in a MPEG version 1 audio frame.
fn l3_read_scale_factors_mpeg1<B: BitStream>(
    bs: &mut B,
    gr: usize,
    ch: usize,
    frame_data: &mut FrameData,
) -> Result<(u32)> {

    let mut bits_read = 0;

    let channel = &frame_data.granules[gr].channels[ch];

    // For MPEG1, scalefac_compress is a 4-bit index into a scale factor bit length lookup table.
    let (slen1, slen2) = SCALE_FACTOR_SLEN[channel.scalefac_compress as usize];

    // Short or Mixed windows...
    if let BlockType::Short { is_mixed } = channel.block_type {
        let data = &mut frame_data.granules[gr].channels[ch];

        // If the block is mixed, there are three total scale factor partitions. The first is a long
        // scale factor partition for bands 0..8 (scalefacs[0..8] with each scale factor being slen1
        // bits long. Following this is a short scale factor partition covering bands 8..11 with a
        // window of 3 (scalefacs[8..17]) and each scale factoring being slen1 bits long.
        //
        // If a block is not mixed, then there are a total of two scale factor partitions. The first
        // is a short scale factor partition for bands 0..6 with a window length of 3
        // (scalefacs[0..18]) and each scale factor being slen1 bits long.
        let n_sfb = if is_mixed { 8 + 3 * 3 } else { 6 * 3 };

        if slen1 > 0 {
            for sfb in 0..n_sfb {
                data.scalefacs[sfb] = bs.read_bits_leq32(slen1)? as u8;
            }
            bits_read += n_sfb * slen1 as usize;
        }

        // The final scale factor partition is always a a short scale factor window. It covers bands
        // 11..17 (scalefacs[17..35]) if the block is mixed, or bands 6..12 (scalefacs[18..36]) if
        // not. Each band has a window of 3 with each scale factor being slen2 bits long.
        if slen2 > 0 {
            for sfb in n_sfb..(n_sfb + (6 * 3)) {
                data.scalefacs[sfb] = bs.read_bits_leq32(slen2)? as u8;
            }
            bits_read += 6 * 3 * slen2 as usize;
        }
    }
    // Normal (long, start, end) windows...
    else {
        // For normal windows there are 21 scale factor bands. These bands are divivided into four
        // band ranges. Scale factors in the first two band ranges: [0..6], [6..11], have scale
        // factors that are slen1 bits long, while the last two band ranges: [11..16], [16..21] have
        // scale factors that are slen2 bits long.
        const SCALE_FACTOR_BANDS: [(usize, usize); 4] = [(0, 6), (6, 11), (11, 16), (16, 21)];

        for (i, (start, end)) in SCALE_FACTOR_BANDS.iter().enumerate() {
            let slen = if i < 2 { slen1 } else { slen2 };

            // Scale factors are already zeroed out, so don't do anything if slen is 0.
            if slen > 0 {
                // The scale factor selection information for this channel indicates that the scale
                // factors should be copied from granule 0 for this channel.
                if gr > 0 && frame_data.scfsi[ch][i] {
                    let (granule0, granule1) = frame_data.granules.split_first_mut().unwrap();

                    granule1[0].channels[ch].scalefacs[*start..*end]
                        .copy_from_slice(&granule0.channels[ch].scalefacs[*start..*end]);
                }
                // Otherwise, read the scale factors from the bitstream.
                else {
                    for sfb in *start..*end {
                        frame_data.granules[gr].channels[ch].scalefacs[sfb] =
                            bs.read_bits_leq32(slen)? as u8;
                    }
                    bits_read += slen as usize * (end - start);
                }
            }
        }
    }

    Ok(bits_read as u32)
}

/// Reads the scale factors for a single channel in a granule in a MPEG version 2 audio frame.
fn l3_read_scale_factors_mpeg2<B: BitStream>(
    bs: &mut B,
    is_intensity_stereo: bool,
    channel: &mut GranuleChannel,
) -> Result<(u32)> {

    let mut bits_read = 0;

    let block_index = match channel.block_type {
        BlockType::Short{ is_mixed: true  } => 2,
        BlockType::Short{ is_mixed: false } => 1,
        _                                   => 0,
    };

    let (slen_table, nsfb_table) = if is_intensity_stereo {
        // The actual value of scalefac_compress is a 9-bit unsigned integer (0..512) for MPEG2. A
        // left shift reduces it to an 8-bit value (0..255).
        let sfc = channel.scalefac_compress as u32 >> 1;

        match sfc {
            0..=179   => ([
                (sfc / 36),
                (sfc % 36) / 6,
                (sfc % 36) % 6,
                0,
            ],
            &SCALE_FACTOR_MPEG2_NSFB[0][block_index]),
            180..=243 => ([
                ((sfc - 180) % 64) >> 4,
                ((sfc - 180) % 16) >> 2,
                ((sfc - 180) %  4),
                0,
            ],
            &SCALE_FACTOR_MPEG2_NSFB[1][block_index]),
            244..=255 => ([
                (sfc - 244) / 3,
                (sfc - 244) % 3,
                0,
                0,
            ],
            &SCALE_FACTOR_MPEG2_NSFB[2][block_index]),
            _ => unreachable!(),
        }
    }
    else {
        // The actual value of scalefac_compress is a 9-bit unsigned integer (0..512) for MPEG2.
        let sfc = channel.scalefac_compress as u32;

        match sfc {
            0..=399   => ([
                (sfc >> 4) / 5,
                (sfc >> 4) % 5,
                (sfc % 16) >> 2,
                (sfc %  4),
            ],
            &SCALE_FACTOR_MPEG2_NSFB[3][block_index]),
            400..=499 => ([
                ((sfc - 400) >> 2) / 5,
                ((sfc - 400) >> 2) % 5,
                (sfc - 400) % 4,
                0,
            ],
            &SCALE_FACTOR_MPEG2_NSFB[4][block_index]),
            500..=512 => ([
                (sfc - 500) / 3,
                (sfc - 500) % 3,
                0,
                0,
            ],
            &SCALE_FACTOR_MPEG2_NSFB[5][block_index]),
            _ => unreachable!(),
        }
    };

    let mut start = 0;

    for (&slen, &n_sfb) in slen_table.iter().zip(nsfb_table.iter()) {
        // If slen > 0, read n_sfb scale factors with each scale factor being slen bits long. If
        // slen == 0, but n_sfb > 0, then the those scale factors should be set to 0. Since all
        // scalefacs are preinitialized to 0, this process may be skipped.
        if slen > 0 {
            for sfb in start..(start + n_sfb) {
                channel.scalefacs[sfb] = bs.read_bits_leq32(slen)? as u8;
            }
            bits_read += slen * n_sfb as u32;
        }

        start += n_sfb;
    }

    Ok(bits_read)
}

/// Reads the Huffman coded spectral samples for a given channel in a granule from a `BitStream`
/// into a provided sample buffer. Returns the number of decoded samples (the starting index of the
/// rzero partition).
///
/// Note, each spectral sample is raised to the (4/3)-rd power. This is not actually part of the
/// Huffman decoding process, but, by converting the integer sample to floating point here we don't
/// need to do pointless casting or use an extra buffer.
fn l3_read_huffman_samples<B: BitStream>(
    bs: &mut B,
    channel: &GranuleChannel,
    part3_bits: u32,
    buf: &mut [f32; 576],
) -> Result<usize> {

    // If there are no Huffman code bits, zero all samples and return immediately.
    if part3_bits == 0 {
        for i in (0..576).step_by(4) {
            buf[i+0] = 0.0;
            buf[i+1] = 0.0;
            buf[i+2] = 0.0;
            buf[i+3] = 0.0;
        }
        return Ok(0);
    }

    // Dereference the POW43 table once per granule since there is a tiny overhead each time a
    // lazy_static is dereferenced that should be amortized over as many samples as possible.
    let pow43_table: &[f32; 8207] = &REQUANTIZE_POW43;

    let mut bits_read = 0;
    let mut i = 0;

    // There are two samples per big_value, therefore multiply big_values by 2 to get number of
    // samples in the big_value partition.
    let big_values_len = 2 * channel.big_values as usize;

    // There are up-to 3 regions in the big_value partition. Determine the sample index denoting the
    // end of each region (non-inclusive). Clamp to the end of the big_values partition.
    let regions: [usize; 3] = [
        min(channel.region1_start as usize, big_values_len),
        min(channel.region2_start as usize, big_values_len),
        min(                           576, big_values_len),
    ];

    // Iterate over each region in big_values.
    for (region_idx, region_end) in regions.iter().enumerate() {

        // Select the Huffman table based on the region's table select value.
        let table = &HUFFMAN_TABLES[channel.table_select[region_idx] as usize];

        // If the table for a region is empty, fill the region with zeros and move on to the next
        // region.
        if table.huff_table.data.is_empty() {
            while i < *region_end {
                buf[i] = 0.0;
                i += 1;
                buf[i] = 0.0;
                i += 1;
            }
            continue;
        }

        // Otherwise, read the big_values.
        while i < *region_end {
            // Decode the next Huffman code.
            let (value, code_len) = bs.read_huffman(&table.huff_table, part3_bits - bits_read)?;
            bits_read += code_len;

            // In the big_values partition, each Huffman code decodes to two sample, x and y. Each
            // sample being 4-bits long.
            let mut x = (value >> 4) as usize;
            let mut y = (value & 0xf) as usize;

            // If the first sample, x, is not 0, further process it.
            if x > 0 {
                // If x is saturated (it is at the maximum possible value), and the table specifies
                // linbits, then read linbits more bits and add it to the sample.
                if x == 15 && table.linbits > 0 {
                    x += bs.read_bits_leq32(table.linbits)? as usize;
                    bits_read += table.linbits;
                }

                // The next bit is the sign bit. The value of the sample is raised to the (4/3)
                // power.
                buf[i] = if bs.read_bit()? { -pow43_table[x] } else { pow43_table[x] };
                bits_read += 1;
            }
            else {
                buf[i] = 0.0;
            }

            i += 1;

            // Likewise, repeat the previous two steps for the second sample, y.
            if y > 0 {
                if y == 15 && table.linbits > 0 {
                    y += bs.read_bits_leq32(table.linbits)? as usize;
                    bits_read += table.linbits;
                }

                buf[i] = if bs.read_bit()? { -pow43_table[y] } else { pow43_table[y] };
                bits_read += 1;
            }
            else {
                buf[i] = 0.0
            }

            i += 1;
        }
    }

    // Select the Huffman table for the count1 partition.
    let count1_table = match channel.count1table_select {
        true => QUADS_HUFFMAN_TABLE_B,
        _    => QUADS_HUFFMAN_TABLE_A,
    };

    // Read the count1 partition.
    while i <= 572 && bits_read < part3_bits {
        // Decode the next Huffman code. Note that we allow the Huffman decoder a few extra bits in 
        // case of a count1 overrun (see below for more details).
        let (value, code_len) = bs.read_huffman(
            &count1_table, 
            part3_bits + count1_table.n_table_bits - bits_read
        )?;
        bits_read += code_len;

        // In the count1 partition, each Huffman code decodes to 4 samples: v, w, x, and y.
        // Each sample is 1-bit long (1 or 0).
        //
        // For each 1-bit sample, if it is 0, then then dequantized sample value is 0 as well. If
        // the 1-bit sample is 1, then read the sign bit (the next bit). The dequantized sample is
        // then either +/-1.0 depending on the sign bit.
        if value & 0x8 != 0 {
            buf[i] = if bs.read_bit()? { -1.0 } else { 1.0 };
            bits_read += 1;
        }
        else {
            buf[i] = 0.0;
        }

        i += 1;

        if value & 0x4 != 0 {
            buf[i] = if bs.read_bit()? { -1.0 } else { 1.0 };
            bits_read += 1;
        }
        else {
            buf[i] = 0.0;
        }

        i += 1;

        if value & 0x2 != 0 {
            buf[i] = if bs.read_bit()? { -1.0 } else { 1.0 };
            bits_read += 1;
        }
        else {
            buf[i] = 0.0;
        }

        i += 1;

        if value & 0x1 != 0 {
            buf[i] = if bs.read_bit()? { -1.0 } else { 1.0 };
            bits_read += 1;
        }
        else {
            buf[i] = 0.0;
        }

        i += 1;
    }

    // Ignore any extra "stuffing" bits.
    if bits_read < part3_bits {
        eprintln!("ignore: {}", part3_bits - bits_read);
        bs.ignore_bits(part3_bits - bits_read)?;
    }
    // Word on the street is that some encoders are poor at "stuffing" bits, resulting in part3_len 
    // being ever so slightly too large. This causes the Huffman decode loop to decode the next few
    // bits as a sample. However, this is random data and not a real sample, so erase it! The caller
    // will be reponsible for re-aligning the bitstream reader. Candy Pop confirms this.
    else if bits_read > part3_bits {
        eprintln!("count1 overrun");
        i -= 4;
    }

    // The final partition after the count1 partition is the rzero partition. Samples in this
    // partition are all 0.
    for j in (i..576).step_by(2) {
        buf[j+0] = 0.0;
        buf[j+1] = 0.0;
    }

    Ok(i)
}

/// Requantize long block samples in `buf`.
fn l3_requantize_long(
    header: &FrameHeader,
    channel: &GranuleChannel,
    buf: &mut [f32],
) {
    // For long blocks dequantization and scaling is governed by the following equation:
    //
    //                     xr(i) = s(i)^(4/3) * 2^(0.25*A) * 2^(-B)
    // where:
    //       s(i) is the decoded Huffman sample
    //      xr(i) is the dequantized sample
    // and:
    //      A = global_gain[gr] - 210
    //      B = scalefac_multiplier * (scalefacs[gr][ch][sfb] + (preflag[gr] * pretab[sfb]))
    //
    // Note: The samples in buf are the result of s(i)^(4/3) for each sample i.

    // The preemphasis table is from table B.6 in ISO/IEC 11172-3.
    const PRE_EMPHASIS: [i32; 22] = [ 
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 
        1, 1, 1, 1, 2, 2, 3, 3, 3, 2, 0,
    ];

    let sfb_indicies = &SCALE_FACTOR_LONG_BANDS[header.sample_rate_idx as usize];

    let mut pow2ab = 0.0;
    
    let scalefac_multiplier = if channel.scalefac_scale { 4 } else { 2 };

    let mut sfb = 0;
    let mut sfb_end = sfb_indicies[sfb] as usize;

    for i in 0..buf.len() {
        // The value of B is dependant on the scale factor band. Therefore, update B only when the
        // scale factor band changes.
        if i == sfb_end {
            let pre_emphasis = if channel.preflag { PRE_EMPHASIS[sfb] } else { 0 };

            // Calculate A.
            let a = channel.global_gain as i32 - 210;

            // Calculate B.
            let b = scalefac_multiplier * (channel.scalefacs[sfb] as i32 + pre_emphasis);

            // Calculate 2^(0.25*A) * 2^(-B). This can be rewritten as 2^{ 0.25 * (A - 4 * B) }.
            // Since scalefac_multiplier was multiplied by 4 above, the final equation becomes 
            // 2^{ 0.25 * (A - B) }.
            pow2ab = f64::powf(2.0, 0.25 * f64::from(a - b)) as f32;

            sfb += 1;
            sfb_end = sfb_indicies[sfb] as usize;
        }

        // Buf contains s(i)^(4/3), now multiply in 2^(0.25*A) * 2^(-B) to get xr(i).
        buf[i] *= pow2ab;
    }
}

/// Requantize short block samples in `buf`.
fn l3_requantize_short(
    header: &FrameHeader,
    channel: &GranuleChannel,
    mut sfb: usize,
    buf: &mut [f32],
) {
    // For short blocks dequantization and scaling is governed by the following equation:
    //
    //                     xr(i) = s(i)^(4/3) * 2^(0.25*A) * 2^(-B)
    // where:
    //       s(i) is the decoded Huffman sample
    //      xr(i) is the dequantized sample
    // and:
    //      A = global_gain[gr] - 210 - (8 * subblock_gain[gr][win])
    //      B = scalefac_multiplier * scalefacs[gr][ch][sfb][win]
    //
    // Note: The samples in buf are the result of s(i)^(4/3) for each sample i.

    let sfb_indicies = &SCALE_FACTOR_SHORT_BANDS[header.sample_rate_idx as usize];

    // Calculate the constant part of A: global_gain[gr] - 210.
    let global_gain = channel.global_gain as i32 - 210;

    // Likweise, the scalefac_multiplier is constant for the granule. The actual scale is multiplied
    // by 4 combine the two pow2 operations into one by adding the exponents. The sum of the
    // exponent is multiplied by 0.25 so B must be multiplied by 4 to counter the quartering.
    let scalefac_mulitplier = if channel.scalefac_scale { 4 } else { 2 };

    let mut i = 0;

    while i < buf.len() {
        // Determine the length of the window (the length of the scale factor band).
        let win_len = (sfb_indicies[sfb+1] - sfb_indicies[sfb]) as usize;

        // Each scale factor band is repeated 3 times over.
        for win in 0..3 {
            // Calculate A.
            let a = global_gain - (8 * channel.subblock_gain[win] as i32);

            // Calculate B.
            let b = scalefac_mulitplier * channel.scalefacs[3*sfb + win] as i32;

            // Calculate 2^(0.25*A) * 2^(-B). This can be rewritten as 2^{ 0.25 * (A - 4 * B) }.
            // Since scalefac_multiplier was multiplied by 4 above, the final equation becomes 
            // 2^{ 0.25 * (A - B) }.
            let pow2ab = f64::powf(2.0,  0.25 * f64::from(a - b)) as f32;

            let win_end = min(buf.len(), i + win_len);

            // Buf contains s(i)^(4/3), now multiply in 2^(0.25*A) * 2^(-B) to get xr(i).
            while i < win_end {
                buf[i] *= pow2ab;
                i += 1;
            }
        }

        sfb += 1;
    }
}

/// Requantize samples in `buf` regardless of block type.
fn l3_requantize(
    header: &FrameHeader,
    channel: &GranuleChannel,
    buf: &mut [f32; 576],
) {
    match channel.block_type {
        BlockType::Short { is_mixed: false } => {
            l3_requantize_short(header, channel, 0, &mut buf[..channel.rzero]);
        },
        BlockType::Short { is_mixed: true } => {
            eprintln!("requantize mixed block.");
            // A mixed block is a combination of a long block and short blocks. The first few scale
            // factor bands, and thus samples, belong to a single long block, while the remaining
            // bands and samples belong to short blocks. Therefore, requantization for mixed blocks
            // can be decomposed into short and long block requantizations.
            //
            // As per ISO/IEC 11172-3, the short scale factor band at which the long block ends and
            // the short blocks begin is denoted by switch_point_s (3). ISO/IEC 13818-3 does not
            // ammend this figure.
            //
            // TODO: Verify if this split makes sense for 8kHz MPEG2.5 bitstreams.
            l3_requantize_long(header, channel, &mut buf[0..36]);
            l3_requantize_short(header, channel, 3, &mut buf[36..channel.rzero]);
        },
        _ => {
            l3_requantize_long(header, channel, &mut buf[..channel.rzero]);
        },
    }
}

/// Reorder samples that are part of short blocks into sub-band order.
fn l3_reorder(
    header: &FrameHeader,
    channel: &GranuleChannel,
    buf: &mut [f32; 576]
) {
    // Only short blocks are reordered.
    if let BlockType::Short { is_mixed } = channel.block_type {
        // Every short block is split into 3 equally sized windows as illustrated below (e.g. for
        // a short scale factor band with win_len=4):
        //
        //    <- Window #1 ->  <- Window #2 ->  <- Window #3 ->
        //   [ 0 | 1 | 2 | 3 ][ 4 | 5 | 6 | 7 ][ 8 | 9 | a | b ]
        //    <-----  3 * Short Scale Factor Band Width  ----->
        //
        // Reordering interleaves the samples of each window as follows:
        //
        //   [ 0 | 4 | 8 | 1 | 5 | 9 | 2 | 6 | a | 3 | 7 | b ]
        //    <----  3 * Short Scale Factor Band Width  ---->
        //
        // Basically, reordering interleaves the 3 windows the same way 3 planar audio buffers
        // would be interleaved.
        debug_assert!(channel.rzero <= 576);

        // TODO: Frankly, this is wasteful... Consider swapping between two internal buffers so we
        // can avoid initializing this to 0 every frame. Again, unsafe is not allowed in codec's so
        // this can't be left uninitialized.
        let mut reorder_buf = [0f32; 576];

        let sfb_bands = &SCALE_FACTOR_SHORT_BANDS[header.sample_rate_idx];

        // Only the short bands in a mixed block are reordered. Adjust the starting scale factor
        // band accordingly.
        //
        // TODO: Verify if this split makes sense for 8kHz MPEG2.5 bitstreams.
        let mut sfb = if is_mixed { 3 } else { 0 };

        let start = 3 * sfb_bands[sfb] as usize;
        let mut i = start;

        while i < channel.rzero {
            // Determine the scale factor band width.
            let win_len = (sfb_bands[sfb+1] - sfb_bands[sfb]) as usize;

            // Respective starting indicies of windows 0, 1, and 2.
            let mut w0 = i;
            let mut w1 = i + 1 * win_len;
            let mut w2 = i + 2 * win_len;

            // Interleave the three windows. This is essentially a matrix transpose.
            // TODO: This could likely be sped up with SIMD. Could this be done in-place?
            for _ in 0..win_len {
                reorder_buf[i+0] = buf[w0];
                w0 += 1;
                reorder_buf[i+1] = buf[w1];
                w1 += 1;
                reorder_buf[i+2] = buf[w2];
                w2 += 1;

                i += 3;
            }

            sfb += 1;
        }

        // Copy reordered samples from the reorder buffer to the actual sample buffer.
        buf[start..i].copy_from_slice(&reorder_buf[start..i]);
    }
}

/// Applies the anti-aliasing filter to sub-bands that are not short blocks.
fn l3_antialias(channel: &GranuleChannel, samples: &mut [f32; 576]) {
    // The number of sub-bands to anti-aliasing depends on block type.
    let sb_end = match channel.block_type {
        // Short blocks are never anti-aliased.
        BlockType::Short { is_mixed: false } => return,
        // Mixed blocks have a long block span the first 36 samples (2 sub-bands). Therefore, only
        // anti-alias these two sub-bands.
        BlockType::Short { is_mixed: true  } =>  2 * 18,
        // All other block types require all 32 sub-bands to be anti-aliased.
        _                                    => 32 * 18,
    };

    // Amortize the lazy_static fetch over the entire anti-aliasing operation.
    let (cs, ca): &([f32; 8], [f32; 8]) = &ANTIALIAS_CS_CA;

    // Anti-aliasing is performed using 8 butterfly calculations at the boundaries of ADJACENT
    // sub-bands. For each calculation, there are two samples: lower and upper. For each iteration,
    // the lower sample index advances backwards from the boundary, while the upper sample index
    // advances forward from the boundary.
    //
    // For example, let B(li, ui) represent the butterfly calculation where li and ui are the
    // indicies of the lower and upper samples respectively. If j is the index of the first sample
    // of a sub-band, then the iterations are as follows:
    //
    // B(j-1,j), B(j-2,j+1), B(j-3,j+2), B(j-4,j+3), B(j-5,j+4), B(j-6,j+5), B(j-7,j+6), B(j-8,j+7)
    //
    // The butterfly calculation itself can be illustrated as follows:
    //
    //              * cs[i]
    //   l0 -------o------(-)------> l1
    //               \    /                  l1 = l0 * cs[i] - u0 * ca[i]
    //                \  / * ca[i]           u1 = u0 * cs[i] + l0 * ca[i]
    //                 \
    //               /  \  * ca[i]           where:
    //             /     \                       cs[i], ca[i] are constant values for iteration i,
    //   u0 ------o------(+)-------> u1          derived from table B.9 of ISO/IEC 11172-3.
    //             * cs[i]
    //
    // Note that all butterfly calculations only involve two samples, and all iterations are
    // independant of each other. This lends itself well for SIMD processing.
    for sb in (18..sb_end).step_by(18) {
        for i in 0..8 {
            let li = sb - 1 - i;
            let ui = sb + i;
            let lower = samples[li];
            let upper = samples[ui];
            samples[li] = lower * cs[i] - upper * ca[i];
            samples[ui] = upper * cs[i] + lower * ca[i];
        }
    }
}

fn l3_stereo(
    header: &FrameHeader,
    granule: &Granule,
    ch: &mut [[f32; 576]; 2],
) -> Result<()> {

    let (ch0, ch1) = {
        let (ch0, ch1) = ch.split_first_mut().unwrap();
        (ch0, &mut ch1[0])
    };

    let (mid_side, intensity) = match header.channels {
        Channels::JointStereo(Mode::Layer3 { mid_side, intensity }) => (mid_side, intensity),
        Channels::JointStereo(Mode::Intensity { .. })               => (false, true),
        _ => (false, false),
    };

    // If mid-side (MS) stereo is used, then the left and right channels are encoded as an average
    // (mid) and difference (side) components.
    //
    // As per ISO/IEC 11172-3, to reconstruct the left and right channels, the following calculation
    // is performed:
    //
    //      l[i] = (m[i] + s[i]) / sqrt(2)
    //      r[i] = (m[i] - s[i]) / sqrt(2)
    // where:
    //      l[i], and r[i] are the left and right channels, respectively.
    //      m[i], and s[i] are the mid and side channels, respectively.
    //
    // In the bitstream, m[i] is transmitted in channel 0, while s[i] in channel 1. After decoding,
    // the left channel replaces m[i] in channel 0, and the right channel replaces s[i] in channel
    // 1.
    if mid_side {
        let end = max(granule.channels[0].rzero, granule.channels[1].rzero);

        for i in 0..end {
            let left = (ch0[i] + ch1[i]) * f32::consts::FRAC_1_SQRT_2;
            let right = (ch0[i] - ch1[i]) * f32::consts::FRAC_1_SQRT_2;
            ch0[i] = left;
            ch1[i] = right;
        }
    }

    // If intensity stereo is used, then samples within the rzero partition are coded using
    // intensity stereo. Intensity stereo codes both channels (left and right) into channel 0.
    // In channel 1, the scale factors, for the scale factor bands within the rzero partition
    // corresponding to the intensity coded bands of channel 0, contain the intensity position.
    // Using the intensity position for each band, the intensity signal may be decoded into left
    // and right channels.
    //
    // As per ISO/IEC 11172-3 and ISO/IEC 13818-3, the following calculation may be performed to
    // decode the intensity coded signal into left and right channels.
    //
    //      l[i] = ch0[i] * k_l
    //      r[i] = ch0[i] * l_r
    // where:
    //      l[i], and r[i] are the left and right channels, respectively.
    //      ch0[i] is the intensity coded signal store in channel 0.
    //      k_l, and k_r are the left and right channel ratios.
    //
    // The channel ratios are dependant on MPEG version. For MPEG1:
    //
    //      r = tan(pos[sfb] * PI/12
    //      k_l = r / (1 + r)
    //      k_r = 1 / (1 + r)
    // where:
    //      pos[sfb] is the position for the scale factor band.
    //
    //  For MPEG2:
    //
    //  If...              | k_l                       | k_r
    //  -------------------+---------------------------+---------------------
    //  pos[sfb]     == 0  | 1.0                       | 1.0
    //  pos[sfb] & 1 == 1  | i0 ^ [(pos[sfb] + 1) / 2] | 1.0
    //  pos[sfb] & 1 == 0  | 1.0                       | i0 ^ (pos[sfb] / 2)
    //
    // where:
    //      pos[sfb] is the position for the scale factor band.
    //      i0 = 1 / sqrt(2)        if (intensity_scale = scalefac_compress & 1) == 1
    //      i0 = 1 / sqrt(sqrt(2))  if (intensity_scale = scalefac_compress & 1) == 0
    //
    // Note: regardless of version, pos[sfb] == 7 is forbidden and indicates intensity stereo
    //       decoding should not be used.
    if intensity {
        eprintln!("INTENSITY");
        // The block types must be the same.
        if granule.channels[0].block_type != granule.channels[1].block_type {
            return decode_error("stereo channel pair block_type mismatch");
        }

        let ch1_rzero = granule.channels[1].rzero as u32;

        // Determine which bands are entirely contained within the rzero partition. Intensity stereo
        // is applied to these bands only.
        match granule.channels[1].block_type {
            // For short blocks, every scale factor band is repeated thrice (for the three windows).
            // Multiply each band start index by 3 before checking if it is above or below the rzero
            // partition.
            BlockType::Short { is_mixed: false } => {
                let short_indicies = &SCALE_FACTOR_SHORT_BANDS[header.sample_rate_idx as usize];

                let short_band = short_indicies[..13].iter()
                                                     .map(|i| 3 * i)
                                                     .position(|i| i >= ch1_rzero);

                if let Some(start) = short_band {
                    l3_intensity_stereo_short(header, &granule.channels[1], start, ch0, ch1);
                }
            },
            // For mixed blocks, the first 36 samples are part of a long block, and the remaining
            // samples are part of short blocks.
            BlockType::Short { is_mixed: true } => {
                let long_indicies = &SCALE_FACTOR_LONG_BANDS[header.sample_rate_idx as usize];

                // Check is rzero begins in the long block.
                let long_band = long_indicies[..8].iter().position(|i| *i >= ch1_rzero);

                // If rzero begins in the long block, then all short blocks are also part of rzero.
                if let Some(start) = long_band {
                    l3_intensity_stereo_long(header, &granule.channels[1], start, 8, ch0, ch1);
                    l3_intensity_stereo_short(header, &granule.channels[1], 3, ch0, ch1);
                }
                // Otherwise, find where rzero begins in the short blocks.
                else {
                    let short_indicies = &SCALE_FACTOR_SHORT_BANDS[header.sample_rate_idx as usize];

                    let short_band = short_indicies[3..13].iter()
                                                          .map(|i| 3 * i)
                                                          .position(|i| i >= ch1_rzero);

                    if let Some(start) = short_band {
                        l3_intensity_stereo_short(header, &granule.channels[1], start, ch0, ch1);
                    }
                };
            },
            // For long blocks, simply find the first scale factor band that is fully in the rzero
            // partition.
            _ => {
                let long_indicies = &SCALE_FACTOR_LONG_BANDS[header.sample_rate_idx as usize];

                let long_band = long_indicies[..22].iter().position(|i| *i >= ch1_rzero);

                if let Some(start) = long_band {
                    l3_intensity_stereo_long(header, &granule.channels[1], start, 22, ch0, ch1);
                }
            },
        }
    }

    Ok(())
}

fn l3_intensity_stereo_short(
    header: &FrameHeader,
    channel: &GranuleChannel,
    sfb_start: usize,
    ch0: &mut [f32; 576],
    ch1: &mut [f32; 576],
) {
    let sfb_indicies = &SCALE_FACTOR_SHORT_BANDS[header.sample_rate_idx as usize];

    // If MPEG1...
    if header.is_mpeg1() {
        for sfb in sfb_start..13 {
            let win_len = (sfb_indicies[sfb+1] - sfb_indicies[sfb]) as usize;

            let mut start = 3 * sfb_indicies[sfb] as usize;

            for win in 0..3 {
                let is_pos = channel.scalefacs[3*sfb + win] as usize;

                if is_pos < 7 {
                    let (ratio_l, ratio_r) = INTENSITY_STEREO_RATIOS[is_pos];

                    // Process each sample within the scale factor band.
                    for i in start..(start + win_len) {
                        let is = ch0[i];
                        ch0[i] = ratio_l * is;
                        ch1[i] = ratio_r * is;
                    }
                }

                start += win_len;
            }
        }
    }
    // Otherwise, if MPEG2 or 2.5...
    else {
        let is_pos_table = &INTENSITY_STEREO_RATIOS_MPEG2[channel.scalefac_compress as usize & 0x1];

        for sfb in sfb_start..13 {
            let win_len = (sfb_indicies[sfb+1] - sfb_indicies[sfb]) as usize;

            let mut start = 3 * sfb_indicies[sfb] as usize;

            for win in 0..3 {
                let is_pos = channel.scalefacs[3*sfb + win] as usize;

                if is_pos != 7 {
                    let (ratio_l, ratio_r) = is_pos_table[is_pos];

                    // Process each sample within the scale factor band.
                    for i in start..(start + win_len) {
                        let is = ch0[i];
                        ch0[i] = ratio_l * is;
                        ch1[i] = ratio_r * is;
                    }
                }

                start += win_len;
            }
        }
    }
}

fn l3_intensity_stereo_long(
    header: &FrameHeader,
    channel: &GranuleChannel,
    sfb_start: usize,
    sfb_end: usize,
    ch0: &mut [f32; 576],
    ch1: &mut [f32; 576],
) {
    let sfb_indicies = &SCALE_FACTOR_LONG_BANDS[header.sample_rate_idx as usize];

    // If MPEG1...
    if header.is_mpeg1() {
        for sfb in sfb_start..sfb_end {
            let is_pos = channel.scalefacs[sfb] as usize;

            // A position of 7 is considered invalid. Additionally, for MPEG1 bitstreams, a scalefac
            // may be up to 4-bits long. A 4 bit scalefac is clearly invalid for intensity coded
            // scale factor bands since the maximum value is 7, but a maliciously crafted file could
            // conceivably make it happen. Therefore, any position > 7 is ignored, thus protecting
            // the table look-up from going out-of-bounds.
            if is_pos < 7 {
                let (ratio_l, ratio_r) = INTENSITY_STEREO_RATIOS[is_pos];

                // Process each sample within the scale factor band.
                let start = sfb_indicies[sfb] as usize;
                let end = sfb_indicies[sfb+1] as usize;

                for i in start..end {
                    let is = ch0[i];
                    ch0[i] = ratio_l * is;
                    ch1[i] = ratio_r * is;
                }
            }
        }
    }
    // Otherwise, if MPEG2 or 2.5...
    else {
        let is_pos_table = &INTENSITY_STEREO_RATIOS_MPEG2[channel.scalefac_compress as usize & 0x1];

        for sfb in sfb_start..sfb_end {
            let is_pos = channel.scalefacs[sfb] as usize;

            // A position of 7 is considered invalid.
            if is_pos != 7 {
                // For MPEG2 bitstreams, a scalefac can be up to 5-bits long and may index the
                // intensity stereo coefficients table directly.
                let (ratio_l, ratio_r) = is_pos_table[is_pos];

                // Process each sample within the scale factor band.
                let start = sfb_indicies[sfb] as usize;
                let end = sfb_indicies[sfb+1] as usize;

                for i in start..end {
                    let is = ch0[i];
                    ch0[i] = ratio_l * is;
                    ch1[i] = ratio_r * is;
                }
            }
        }
    }
}

/// Performs the 12-point IMDCT, and windowing for each of the 3 short windows of a short block, and
/// then overlap-adds the result.
fn l3_imdct12_win(x: &[f32], window: &[f32; 36], out: &mut [f32; 36]) {
    debug_assert!(x.len() == 18);

    let cos12 = &IMDCT_COS_12;

    for w in 0..3 {
        for i in 0..12 {
            // Apply a 12-point (N=12) IMDCT for each of the 3 short windows.
            //
            // The IMDCT is defined as:
            //
            //        (N/2)-1
            // y[i] =   SUM   { x[k] * cos(PI/2N * (2i + 1 + N/2) * (2k + 1)) }
            //          k=0
            //
            // For N=12, the IMDCT becomes:
            //
            //         5
            // y[i] = SUM { x[k] * cos(PI/24 * (2i + 7) * (2k + 1)) }
            //        k=0
            //
            // The value of cos(..) is easily indexable by i and k, and is therefore pre-computed
            // and placed in a look-up table.
            let y = (x[3*0 + w] * cos12[i][0])
                        + (x[3*1 + w] * cos12[i][1])
                        + (x[3*2 + w] * cos12[i][2])
                        + (x[3*3 + w] * cos12[i][3])
                        + (x[3*4 + w] * cos12[i][4])
                        + (x[3*5 + w] * cos12[i][5]);

            // Each adjacent 12-point IMDCT window is overlapped and added in the output, with the
            // first and last 6 samples of the output are always being 0.
            //
            // In the above calculation, y is the result of the 12-point IMDCT for sample i. For the
            // following description, assume the 12-point IMDCT result is y[0..12], where the value
            // y calculated above is y[i].
            //
            // Each sample in the IMDCT is multiplied by the appropriate window function as
            // specified in ISO/IEC 11172-3. The values of the window function are pre-computed and
            // given by window[0..12].
            //
            // Since there are 3 IMDCT windows (indexed by w), y[0..12] is calculated 3 times.
            // For the purpose of the diagram below, we label these IMDCT windows as: y0[0..12],
            // y1[0..12], and y2[0..12], for IMDCT windows 0..3 respectively.
            //
            // Therefore, the overlap-and-add operation can be visualized as below:
            //
            // 0             6           12           18           24           30            36
            // +-------------+------------+------------+------------+------------+-------------+
            // |      0      |  y0[..6]   |  y0[..6]   |  y1[6..]   |  y2[6..]   |      0      |
            // |     (6)     |            |  + y1[6..] |  + y2[..6] |            |     (6)     |
            // +-------------+------------+------------+------------+------------+-------------+
            // .             .            .            .            .            .             .
            // .             +-------------------------+            .            .             .
            // .             |      IMDCT #1 (y0)      |            .            .             .
            // .             +-------------------------+            .            .             .
            // .             .            +-------------------------+            .             .
            // .             .            |      IMDCT #2 (y1)      |            .             .
            // .             .            +-------------------------+            .             .
            // .             .            .            +-------------------------+             .
            // .             .            .            |      IMDCT #3 (y2)      |             .
            // .             .            .            +-------------------------+             .
            // .             .            .            .            .            .             .
            out[6 + 6*w + i] += y * window[i];
        }
    }
}

fn l3_hybrid_synthesis(
    channel: &GranuleChannel,
    overlap: &mut [[f32; 18]; 32],
    samples: &mut [f32; 576],
) {
    let imdct_windows = &IMDCT_WINDOWS;

    // If the block is short, the 12-point IMDCT must be used.
    if let BlockType::Short { is_mixed } = channel.block_type {
        // There are 32 sub-bands. If the block is mixed, then the first two sub-bands are windowed
        // as long blocks, while the rest are windowed as short blocks. The following arrays
        // indicate the index of the window to use within imdct_windows. Each element is repeated
        // twice (i.e., sub-bands 0 and 1 use win_idx[0], 2 and 3 use win_idx[1], ..).
        let win_idx = match is_mixed {
            true => &[0, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3],
            _    => &[3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3],
        };

        // For each of the 32 sub-bands (18 samples each)...
        for sb in 0..32 {
            let start = 18 * sb;

            // Get the window for the sub-band since with short blocks it may change per sub-band.
            let window = &imdct_windows[win_idx[sb >> 1]];

            // Perform the 12-point IMDCT on each of the 3 short block windows.
            let mut output = [0f32; 36];
            l3_imdct12_win(&samples[start..(start + 18)], window, &mut output);

            // Overlap the lower half of the IMDCT output (values 0..18) with the upper values of
            // the IMDCT (values 18..36) of the /previous/ iteration of the IMDCT.
            for i in (0..18).step_by(2) {
                samples[start + (i+0)] = overlap[sb][i+0] + output[i+0];
                overlap[sb][i+0] = output[18 + i+0];

                samples[start + (i+1)] = overlap[sb][i+1] + output[i+1];
                overlap[sb][i+1] = output[18 + i+1];
            }
        }
    }
    // Otherwise, all other blocks use the 36-point IMDCT.
    else {
        let mut output = [0f32; 36];

        // Select the appropriate window given the block type.
        let window = match channel.block_type {
            BlockType::Long  => &imdct_windows[0],
            BlockType::Start => &imdct_windows[1],
            BlockType::End   => &imdct_windows[2],
            // Short blocks are handled above.
            _                => unreachable!(),
        };

        // For each of the 32 sub-bands (18 samples each)...
        for sb in 0..32 {
            let start = 18 * sb;

            // Perform the 36-point on the entire long block.
            imdct36::imdct36(&samples[start..(start + 18)], &mut output);

            // Overlap the lower half of the IMDCT output (values 0..18) with the upper values of
            // the IMDCT (values 18..36) of the /previous/ iteration of the IMDCT. While doing this
            // also apply the window.
            for i in (0..18).step_by(2) {
                samples[start + (i+0)] = overlap[sb][i+0] + (output[i+0] * window[i+0]);
                overlap[sb][i+0] = output[18 + i+0] * window[18 + i+0];

                samples[start + (i+1)] = overlap[sb][i+1] + (output[i+1] * window[i+1]);
                overlap[sb][i+1] = output[18 + i+1] * window[18 + i+1];
            }
        }
    }
}

/// Inverts odd samples in odd sub-bands.
fn l3_frequency_inversion(samples: &mut [f32; 576]) {
    // There are 32 sub-bands spanning 576 samples:
    //
    //        0    18    36    54    72    90   108       558    576
    //        +-----+-----+-----+-----+-----+-----+ . . . . +------+
    // s[i] = | sb0 | sb1 | sb2 | sb3 | sb4 | sb5 | . . . . | sb31 |
    //        +-----+-----+-----+-----+-----+-----+ . . . . +------+
    // 
    // The odd sub-bands are thusly:
    //
    //      sb1  -> s[ 18.. 36]
    //      sb3  -> s[ 54.. 72]
    //      sb5  -> s[ 90..108]
    //      ...
    //      sb31 -> s[558..576]
    // 
    // Each odd sample in the aforementioned sub-bands must be negated.
    for i in (18..576).step_by(36) {
        // Sample negation is unrolled into a 2x4 + 1 (9) operation to improve vectorization.
        for j in (i..i+16).step_by(8) {
            samples[j+1] = -samples[j+1];
            samples[j+3] = -samples[j+3];
            samples[j+5] = -samples[j+5];
            samples[j+7] = -samples[j+7];
        }
        samples[i+18-1] = -samples[i+18-1];
    }
}

/// Reads the main_data portion of a MPEG audio frame from a `BitStream` into `FrameData`.
fn l3_read_main_data(
    header: &FrameHeader,
    frame_data: &mut FrameData,
    state: &mut State,
) -> Result<()> {

    let main_data = state.resevoir.bytes_ref();
    let mut part2_3_begin = 0;

    for gr in 0..header.n_granules() {
        for ch in 0..header.n_channels() {
            // This is an unfortunate workaround for something that should be fixed in BitStreamLtr.
            // This code repositions the bitstream exactly at the intended start of the next part2_3
            // data. This is to fix files that overread in the Huffman decoder.
            //
            // TODO: Implement a rewind on the BitStream to undo the last read.
            let byte_index = part2_3_begin >> 3;
            let bit_index = part2_3_begin & 0x7;

            let mut bs = BitStreamLtr::new(BufStream::new(&main_data[byte_index..]));

            if bit_index > 0 {
                bs.ignore_bits(bit_index as u32)?;
            }

            // Read the scale factors (part2) and get the number of bits read. For MPEG version 1...
            let part2_len = if header.is_mpeg1() {
                l3_read_scale_factors_mpeg1(&mut bs, gr, ch, frame_data)
            }
            // For MPEG version 2...
            else {
                l3_read_scale_factors_mpeg2(
                    &mut bs,
                    ch > 0 && header.is_intensity_stereo(),
                    &mut frame_data.granules[gr].channels[ch])
            }?;

            let part2_3_length = frame_data.granules[gr].channels[ch].part2_3_length as u32;

            // The length part2 must be less than or equal to the part2_3_length.
            if part2_len > part2_3_length {
                return decode_error("part2_3_length is not valid");
            }

            // The Huffman code length (part3).
            let part3_len = part2_3_length - part2_len;
            
            // Decode the Huffman coded spectral samples and get the starting index of the rzero
            // partition.
            frame_data.granules[gr].channels[ch].rzero = l3_read_huffman_samples(
                &mut bs,
                &frame_data.granules[gr].channels[ch],
                part3_len,
                &mut state.samples[gr][ch],
            )?;

            part2_3_begin += part2_3_length as usize;
        }
    }

    Ok(())
}


/// `BitResevoir` implements the bit resevoir mechanism for main_data. Since frames have a
/// deterministic length based on the bit-rate, low-complexity portions of the audio may not need
/// every byte allocated to the frame. The bit resevoir mechanism allows these unused portions of
/// frames to be used by future frames.
pub struct BitResevoir {
    buf: Box<[u8]>,
    len: usize,
}

impl BitResevoir {
    pub fn new() -> Self {
        BitResevoir {
            buf: vec![0u8; 2048].into_boxed_slice(),
            len: 0,
        }
    }

    fn fill<B: Bytestream>(
        &mut self,
        reader: &mut B,
        main_data_begin: usize,
        main_data_size: usize) -> Result<()>
    {
        // The value `main_data_begin` indicates the number of bytes from the previous frames to
        // reuse. It must be less than or equal to the amount of bytes in the buffer.
        if main_data_begin > self.len {
            return decode_error("Invalid main_data_begin offset.");
        }

        // Shift the reused bytes to the beginning of the resevoir.
        // TODO: For Rust 1.37, use copy_within() for more efficient overlapping copies.
        // self.buf.copy_within(self.len - main_data_begin..self.len, 0);
        let prev = self.len - main_data_begin;
        for i in 0..main_data_begin {
            self.buf[i] = self.buf[prev + i];
        }

        // Read the remaining amount of bytes.
        let main_data_end = main_data_begin + main_data_size;
        reader.read_buf_bytes(&mut self.buf[main_data_begin..main_data_end])?;
        self.len = main_data_end;

        Ok(())
    }

    fn bytes_ref(&self) -> &[u8] {
        &self.buf[..self.len]
    }
}

/// MP3 depends on the state of the previous frame to decode the next. `State` is a structure
/// containing all the stateful information required to decode the next frame.
pub struct State {
    samples: [[[f32; 576]; 2]; 2],
    overlap: [[[f32; 18]; 32]; 2],
    synthesis: [synthesis::SynthesisState; 2],
    resevoir: BitResevoir,
}

impl State {
    pub fn new() -> Self {
        State {
            samples: [[[0f32; 576]; 2]; 2],
            overlap: [[[0f32; 18]; 32]; 2],
            synthesis: Default::default(),
            resevoir: BitResevoir::new(),
        }
    }
}

/// Process the next MPEG audio frame from the stream.
pub fn decode_frame<B: Bytestream>(
    reader: &mut B,
    header: &FrameHeader,
    state: &mut State,
    out: &mut AudioBuffer<f32>,
) -> Result<()> {

    // Clear the audio output buffer.
    out.clear();

    // Choose decode steps based on layer.
    match header.layer {
        MpegLayer::Layer3 => {
            // Initialize an empty FrameData to store the side_info and main_data portions of the
            // frame.
            let mut frame_data: FrameData = Default::default();

            // Read side_info into the frame data.
            // TODO: Use a MonitorStream to compute the CRC.
            let side_info_len = l3_read_side_info(reader, &header, &mut frame_data)?;

            // Buffer main_data into the bit resevoir.
            state.resevoir.fill(
                reader,
                frame_data.main_data_begin as usize,
                header.frame_size - side_info_len
            )?;

            l3_read_main_data(&header, &mut frame_data, state)?;

            for gr in 0..header.n_granules() {
                // Each granule will yield 576 samples.
                out.render_reserved(Some(576));

                let granule = &frame_data.granules[gr];

                // Requantize all non-zero (big_values and count1 partition) spectral samples.
                l3_requantize(&header, &granule.channels[0], &mut state.samples[gr][0]);

                // If there is more than one channel: requantize the second channel and then apply 
                // joint stereo processing.
                if header.channels != Channels::Mono {
                    l3_requantize(&header, &granule.channels[1], &mut state.samples[gr][1]);
                    l3_stereo(&header, &granule, &mut state.samples[gr])?;
                }

                // The remaining steps are channel independant.
                for ch in 0..header.n_channels() {
                    // Reorder any spectral samples in short blocks into sub-band order.
                    l3_reorder(&header, &granule.channels[ch], &mut state.samples[gr][ch]);

                    // Apply the anti-aliasing filter to blocks that are not short.
                    l3_antialias(&granule.channels[ch], &mut state.samples[gr][ch]);

                    // Perform hybrid-synthesis (IMDCT and windowing).
                    l3_hybrid_synthesis(
                        &granule.channels[ch],
                        &mut state.overlap[ch],
                        &mut state.samples[gr][ch],
                    );

                    // Invert to odd samples in odd sub-bands.
                    l3_frequency_inversion(&mut state.samples[gr][ch]);

                    let out_ch_samples = out.chan_mut(ch as u8);

                    // Perform polyphase synthesis.
                    synthesis::synthesis(
                        &mut state.samples[gr][ch],
                        &mut state.synthesis[ch],
                        &mut out_ch_samples[(gr * 576)..((gr + 1) * 576)],
                    );
                }
            }
        },
        _ => return unsupported_error("Unsupported MPEG Layer."),
    }

    Ok(())
}

mod imdct36 {
    /// Performs an Inverse Modified Discrete Cosine Transform (IMDCT) transforming 18
    /// frequency-domain input samples, into 36 time-domain output samples.
    ///
    /// This is a straight-forward implementation of the IMDCT using Szu-Wei Lee's algorithm
    /// published in article [1].
    ///
    /// [1] Szu-Wei Lee, "Improved algorithm for efficient computation of the forward and backward
    /// MDCT in MPEG audio coder", IEEE Transactions on Circuits and Systems II: Analog and Digital
    /// Signal Processing, vol. 48, no. 10, pp. 990-994, 2001.
    ///
    /// https://ieeexplore.ieee.org/document/974789
    pub fn imdct36(x: &[f32], y: &mut [f32; 36]) {
        let mut t = [0f32; 18];

        dct_iv(x, &mut t);

        // Mapping of DCT-IV to IMDCT
        //
        //  0            9                       18           36
        //  +------------+------------------------+------------+
        //  | dct[9..18] | -dct[0..18].rev()      | -dct[0..9] |
        //  +------------+------------------------+------------+
        //
        // where dct[] is the DCT-IV of x.

        // First 9 IMDCT values are values 9..18 in the DCT-IV.
        for i in (0..9).step_by(3) {
            y[i+0] = t[9 + (i+0)];
            y[i+1] = t[9 + (i+1)];
            y[i+2] = t[9 + (i+2)];
        }

        // Next 18 IMDCT values are negated and /reversed/ values 0..18 in the DCT-IV.
        for i in (9..27).step_by(3) {
            y[i+0] = -t[27 - (i+0) - 1];
            y[i+1] = -t[27 - (i+1) - 1];
            y[i+2] = -t[27 - (i+2) - 1];
        }

        // Last 9 IMDCT values are negated values 0..9 in the DCT-IV.
        for i in (27..36).step_by(3) {
            y[i+0] = -t[(i+0) - 27];
            y[i+1] = -t[(i+1) - 27];
            y[i+2] = -t[(i+2) - 27];
        }
    }

    /// Continutation of `imdct36`.
    ///
    /// Step 2: Mapping N/2-point DCT-IV to N/2-point SDCT-II.
    fn dct_iv(x: &[f32], y: &mut [f32; 18]) {
        debug_assert!(x.len() == 18);

        // Scale factors for input samples. Computed from (16).
        // 2 * cos(PI * (2*m + 1) / (2*36)
        const SCALE: [f32; 18] = [
            1.9980964431637156,  // m=0
            1.9828897227476208,  // m=1
            1.9525920142398667,  // m=2
            1.9074339014964539,  // m=3
            1.8477590650225735,  // m=4
            1.7740216663564434,  // m=5
            1.6867828916257714,  // m=6
            1.5867066805824706,  // m=7
            1.4745546736202479,  // m=8
            1.3511804152313207,  // m=9
            1.2175228580174413,  // m=10
            1.0745992166936478,  // m=11
            0.9234972264700677,  // m=12
            0.7653668647301797,  // m=13
            0.6014115990085461,  // m=14
            0.4328792278762058,  // m=15
            0.2610523844401030,  // m=16
            0.0872387747306720,  // m=17
        ];

        let samples = [
            SCALE[ 0] * x[ 0],
            SCALE[ 1] * x[ 1],
            SCALE[ 2] * x[ 2],
            SCALE[ 3] * x[ 3],
            SCALE[ 4] * x[ 4],
            SCALE[ 5] * x[ 5],
            SCALE[ 6] * x[ 6],
            SCALE[ 7] * x[ 7],
            SCALE[ 8] * x[ 8],
            SCALE[ 9] * x[ 9],
            SCALE[10] * x[10],
            SCALE[11] * x[11],
            SCALE[12] * x[12],
            SCALE[13] * x[13],
            SCALE[14] * x[14],
            SCALE[15] * x[15],
            SCALE[16] * x[16],
            SCALE[17] * x[17],
        ];

        sdct_ii_18(&samples, y);

        y[0] /= 2.0;
        // Hopefully this is vectorized...
        for i in (1..17).step_by(4) {
            y[i+0] = (y[i+0] / 2.0) - y[i+0-1];
            y[i+1] = (y[i+1] / 2.0) - y[i+1-1];
            y[i+2] = (y[i+2] / 2.0) - y[i+2-1];
            y[i+3] = (y[i+3] / 2.0) - y[i+3-1];
        }
        y[17] = (y[17] / 2.0) - y[16];
    }

    /// Continutation of `imdct36`.
    ///
    /// Step 3: Decompose N/2-point SDCT-II into two N/4-point SDCT-IIs.
    fn sdct_ii_18(x: &[f32; 18], y: &mut [f32; 18]) {
        // Scale factors for odd input samples. Computed from (23).
        // 2 * cos(PI * (2*m + 1) / 36)
        const SCALE: [f32; 9] = [
            1.9923893961834911,  // m=0
            1.9318516525781366,  // m=1
            1.8126155740732999,  // m=2
            1.6383040885779836,  // m=3
            1.4142135623730951,  // m=4
            1.1471528727020923,  // m=5
            0.8452365234813989,  // m=6
            0.5176380902050419,  // m=7
            0.1743114854953163,  // m=8
        ];

        let even = [
            x[0] + x[18 - 1],
            x[1] + x[18 - 2],
            x[2] + x[18 - 3],
            x[3] + x[18 - 4],
            x[4] + x[18 - 5],
            x[5] + x[18 - 6],
            x[6] + x[18 - 7],
            x[7] + x[18 - 8],
            x[8] + x[18 - 9],
        ];

        sdct_ii_9(&even, y);

        let odd = [
            SCALE[0] * (x[0] - x[18 - 1]),
            SCALE[1] * (x[1] - x[18 - 2]),
            SCALE[2] * (x[2] - x[18 - 3]),
            SCALE[3] * (x[3] - x[18 - 4]),
            SCALE[4] * (x[4] - x[18 - 5]),
            SCALE[5] * (x[5] - x[18 - 6]),
            SCALE[6] * (x[6] - x[18 - 7]),
            SCALE[7] * (x[7] - x[18 - 8]),
            SCALE[8] * (x[8] - x[18 - 9]),
        ];

        sdct_ii_9(&odd, &mut y[1..]);

        y[ 3] -= y[ 3 - 2];
        y[ 5] -= y[ 5 - 2];
        y[ 7] -= y[ 7 - 2];
        y[ 9] -= y[ 9 - 2];
        y[11] -= y[11 - 2];
        y[13] -= y[13 - 2];
        y[15] -= y[15 - 2];
        y[17] -= y[17 - 2];
    }

    /// Continutation of `imdct36`.
    ///
    /// Step 4: Computation of 9-point (N/4) SDCT-II.
    fn sdct_ii_9(x: &[f32; 9], y: &mut [f32]) {
        const D: [f32; 7] = [
            -1.7320508075688772,  // -sqrt(3.0)
             1.8793852415718166,  // -2.0 * cos(8.0 * PI / 9.0)
            -0.3472963553338608,  // -2.0 * cos(4.0 * PI / 9.0)
            -1.5320888862379560,  // -2.0 * cos(2.0 * PI / 9.0)
            -0.6840402866513378,  // -2.0 * sin(8.0 * PI / 9.0)
            -1.9696155060244160,  // -2.0 * sin(4.0 * PI / 9.0)
            -1.2855752193730785,  // -2.0 * sin(2.0 * PI / 9.0)
        ];

        let a01 = x[3] + x[5];
        let a02 = x[3] - x[5];
        let a03 = x[6] + x[2];
        let a04 = x[6] - x[2];
        let a05 = x[1] + x[7];
        let a06 = x[1] - x[7];
        let a07 = x[8] + x[0];
        let a08 = x[8] - x[0];

        let a09 = x[4] + a05;
        let a10 = a01 + a03;
        let a11 = a10 + a07;
        let a12 = a03 - a07;
        let a13 = a01 - a07;
        let a14 = a01 - a03;
        let a15 = a02 - a04;
        let a16 = a15 + a08;
        let a17 = a04 + a08;
        let a18 = a02 - a08;
        let a19 = a02 + a04;
        let a20 = 2.0 * x[4] - a05;

        let m1 = D[0] * a06;
        let m2 = D[1] * a12;
        let m3 = D[2] * a13;
        let m4 = D[3] * a14;
        let m5 = D[0] * a16;
        let m6 = D[4] * a17;
        let m7 = D[5] * a18;  // Note: the cited paper has an error, a1 should be a18.
        let m8 = D[6] * a19;

        let a21 = a20 + m2;
        let a22 = a20 - m2;
        let a23 = a20 + m3;
        let a24 = m1 + m6;
        let a25 = m1 - m6;
        let a26 = m1 + m7;

        y[ 0] = a09 + a11;
        y[ 2] = m8 - a26;
        y[ 4] = m4 - a21;
        y[ 6] = m5;
        y[ 8] = a22 - m3;
        y[10] = a25 - m7;
        y[12] = a11 - 2.0 * a09;
        y[14] = a24 + m8;
        y[16] = a23 + m4;
    }


    #[cfg(test)]
    mod tests {
        use super::imdct36;
        use std::f64;

        fn imdct36_analytical(x: &[f32; 18]) -> [f32; 36] {
            let mut result = [0f32; 36];

            const PI_72: f64 = f64::consts::PI / 72.0;

            for i in 0..36 {
                let mut sum = 0.0;
                for j in 0..18 {
                    sum += (x[j] as f64) * (PI_72 * (((2*i) + 1 + 18) * ((2*j) + 1)) as f64).cos();
                }
                result[i] = sum as f32;
            }
            result
        }

        #[test]
        fn verify_imdct36() {
            const TEST_VECTOR: [f32; 18] = [ 
                0.0976, 0.9321, 0.6138, 0.0857, 0.0433, 0.4855, 0.2144, 0.8488,
                0.6889, 0.2983, 0.1957, 0.7037, 0.0052, 0.0197, 0.3188, 0.5123,
                0.2994, 0.7157,
            ];

            let mut test_result = [0f32; 36];
            imdct36(&TEST_VECTOR, &mut test_result);

            let actual_result = imdct36_analytical(&TEST_VECTOR);
            for i in 0..36 {
                assert!((actual_result[i] - test_result[i]).abs() < 0.00001);
            }
        }
    }

}
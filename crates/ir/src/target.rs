// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Hazuki Keatsu

use std::error::Error;
use std::fmt;

/// The byte order of multi-byte data in memory
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Endianness {
    Little,
    Big,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TargetSpec {
    /// LLVM target triple, such as "i686-unknown-linux-gnu"
    triple: String,
    /// LLVM target machine, such as "generic", "apple-m1"
    cpu: String,
    /// CPU command feature, such as "+avx2"
    features: String,
    data_layout: String,
    pointer_width: u8,
    endianness: Endianness,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TargetSpecError {
    MissingTriple,
    MissingDataLayout,
    InvalidPointerWidth(u8),
}

impl TargetSpec {
    pub fn new(
        triple: impl Into<String>,
        cpu: impl Into<String>,
        features: impl Into<String>,
        data_layout: impl Into<String>,
        pointer_width: u8,
        endianness: Endianness,
    ) -> Result<Self, TargetSpecError> {
        let triple = triple.into();
        let data_layout = data_layout.into();
        if triple.is_empty() {
            return Err(TargetSpecError::MissingTriple);
        }
        if data_layout.is_empty() {
            return Err(TargetSpecError::MissingDataLayout);
        }
        if !matches!(pointer_width, 32 | 64) {
            return Err(TargetSpecError::InvalidPointerWidth(pointer_width));
        }

        Ok(Self {
            triple,
            cpu: cpu.into(),
            features: features.into(),
            data_layout,
            pointer_width,
            endianness,
        })
    }

    pub fn for_test_32() -> Self {
        Self::new(
            "i686-unknown-linux-gnu",
            "generic",
            "",
            "e-m:e-p:32:32-i64:64-n8:16:32-S128",
            32,
            Endianness::Little,
        )
        .expect("the 32-bit test target must be valid")
    }

    pub fn for_test_64() -> Self {
        Self::new(
            "x86_64-unknown-linux-gnu",
            "generic",
            "",
            "e-m:e-p:64:64-i64:64-n8:16:32:64-S128",
            64,
            Endianness::Little,
        )
        .expect("the 64-bit test target must be valid")
    }

    pub fn triple(&self) -> &str {
        &self.triple
    }

    pub fn cpu(&self) -> &str {
        &self.cpu
    }

    pub fn features(&self) -> &str {
        &self.features
    }

    pub fn data_layout(&self) -> &str {
        &self.data_layout
    }

    pub const fn pointer_width(&self) -> u8 {
        self.pointer_width
    }

    pub const fn endianness(&self) -> Endianness {
        self.endianness
    }
}

impl fmt::Display for TargetSpecError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingTriple => formatter.write_str("target triple must not be empty"),
            Self::MissingDataLayout => formatter.write_str("target data layout must not be empty"),
            Self::InvalidPointerWidth(width) => {
                write!(
                    formatter,
                    "target pointer width must be 32 or 64, got {width}"
                )
            }
        }
    }
}

impl Error for TargetSpecError {}

// Copyright (c) 2022 Espresso Systems (espressosys.com)
// This file is part of the Espresso library.
//
// This program is free software: you can redistribute it and/or modify it under the terms of the GNU
// General Public License as published by the Free Software Foundation, either version 3 of the
// License, or (at your option) any later version.
// This program is distributed in the hope that it will be useful, but WITHOUT ANY WARRANTY; without
// even the implied warranty of MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See the GNU
// General Public License for more details.
// You should have received a copy of the GNU General Public License along with this program. If not,
// see <https://www.gnu.org/licenses/>.

pub mod network;
#[cfg(any(test, feature = "mocks"))]
pub mod testing;

use espresso_core::ledger::EspressoLedger;

pub use seahorse::*;

pub type EspressoKeystore<'a, Backend, Meta> = Keystore<'a, Backend, EspressoLedger, Meta>;
pub type EspressoKeystoreError = KeystoreError<EspressoLedger>;

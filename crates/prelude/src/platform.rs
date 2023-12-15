// Copyright 2023 Google LLC
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Provides API to interact with the platform.

use alloc::boxed::Box;
use alloc::vec;

use wasefire_applet_api::platform as api;
use wasefire_sync::Mutex;

pub mod update;

/// Returns the version of the platform.
pub fn version() -> &'static [u8] {
    static VERSION: Mutex<Option<&'static [u8]>> = Mutex::new(None);
    let mut guard = VERSION.lock();
    if let Some(version) = *guard {
        return version;
    }
    let params = api::version::Params { ptr: core::ptr::null_mut(), len: 0 };
    let api::version::Results { len } = unsafe { api::version(params) };
    let mut version = vec![0; len];
    let params = api::version::Params { ptr: version[..].as_mut_ptr(), len };
    let api::version::Results { .. } = unsafe { api::version(params) };
    let version = Box::leak(version.into_boxed_slice());
    *guard = Some(version);
    version
}

/// Reboots the device (thus platform and applets).
pub fn reboot() -> ! {
    let api::reboot::Results { res } = unsafe { api::reboot() };
    unreachable!("reboot() returned {res}");
}

// Copyright 2024 Google LLC
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

use std::fmt::Display;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::str::FromStr;

use anyhow::{bail, ensure, Result};
use cargo_metadata::{Metadata, MetadataCommand};
use clap::{ValueEnum, ValueHint};
use data_encoding::HEXLOWER_PERMISSIVE as HEX;
use rusb::{GlobalContext, UsbContext};
use wasefire_protocol::{self as service, applet, Api};
use wasefire_protocol_usb::Connection;

use crate::{cmd, fs};

/// Platform information.
pub type PlatformInfo = wasefire_wire::Yoke<service::platform::Info<'static>>;

/// Options to connect to a platform.
#[derive(Clone, clap::Args)]
pub struct ConnectionOptions {
    /// Serial of the platform to connect to (in hexadecimal).
    #[arg(long, env = "WASEFIRE_SERIAL")]
    serial: Option<Hex>,

    /// Timeout to send or receive on the platform protocol.
    #[arg(long, default_value = "1s")]
    timeout: humantime::Duration,
}

#[derive(Clone)]
struct Hex(Vec<u8>);

impl Display for Hex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        HEX.encode(&self.0).fmt(f)
    }
}

impl FromStr for Hex {
    type Err = data_encoding::DecodeError;
    fn from_str(input: &str) -> std::result::Result<Self, Self::Err> {
        Ok(Hex(HEX.decode(input.as_bytes())?))
    }
}

impl ConnectionOptions {
    /// Establishes a connection.
    pub fn connect(&self) -> Result<Connection<GlobalContext>> {
        let context = GlobalContext::default();
        let mut matches = Vec::new();
        let serial = self.serial.as_ref();
        for candidate in wasefire_protocol_usb::list(&context)? {
            let connection = candidate.connect(*self.timeout)?;
            let info = connection.call::<service::PlatformInfo>(())?;
            if serial.map_or(false, |x| x.0 != info.get().serial) {
                continue;
            }
            matches.push((connection, info));
        }
        match matches.len() {
            1 => Ok(matches.pop().unwrap().0),
            0 => match serial {
                None => bail!("no connected platforms"),
                Some(serial) => bail!("no connected platforms with serial={serial}"),
            },
            _ => match serial {
                None => {
                    eprintln!("Choose one of the connected platforms using its serial:");
                    for (_, info) in matches {
                        eprintln!("--serial={}", HEX.encode(info.get().serial));
                    }
                    bail!("more than one connected platform");
                }
                Some(serial) => {
                    eprintln!("Multiple connected platforms with serial={serial}:");
                    for (connection, _) in matches {
                        eprintln!("- {connection}");
                    }
                    bail!("more than one connected platform with serial={serial}");
                }
            },
        }
    }
}

/// Parameters for an applet or platform RPC.
#[derive(clap::Args)]
pub struct Rpc {
    /// Reads the request from this file instead of standard input.
    #[arg(long, value_hint = ValueHint::FilePath)]
    input: Option<PathBuf>,

    /// Writes the response to this file instead of standard output.
    #[arg(long, value_hint = ValueHint::AnyPath)]
    output: Option<PathBuf>,
}

impl Rpc {
    fn read(&self) -> Result<Vec<u8>> {
        match &self.input {
            Some(path) => fs::read(path),
            None => fs::read_stdin(),
        }
    }

    fn write(&self, response: &[u8]) -> Result<()> {
        match &self.output {
            Some(path) => fs::write(path, response),
            None => fs::write_stdout(response),
        }
    }
}

/// Calls an RPC to an applet on a platform.
#[derive(clap::Args)]
pub struct AppletRpc {
    /// Applet identifier in the platform.
    applet: Option<String>,

    #[clap(flatten)]
    rpc: Rpc,

    /// Number of retries to receive a response.
    #[arg(long, default_value = "3")]
    retries: usize,
}

impl AppletRpc {
    pub fn run<T: UsbContext>(self, connection: &Connection<T>) -> Result<()> {
        let AppletRpc { applet, rpc, retries } = self;
        let applet_id = match applet {
            Some(_) => bail!("applet identifiers are not supported yet"),
            None => applet::AppletId,
        };
        let request = applet::Request { applet_id, request: &rpc.read()? };
        connection.call::<service::AppletRequest>(request)?.get();
        for _ in 0 .. retries {
            let response = connection.call::<service::AppletResponse>(applet_id)?;
            if let Some(response) = response.get().response {
                return rpc.write(response);
            }
        }
        bail!("did not receive a response after {retries} retries");
    }
}

/// Lists the connected platforms.
#[derive(clap::Args)]
pub struct PlatformList {
    /// Timeout to send or receive on the platform protocol.
    #[arg(long, default_value = "1s")]
    timeout: humantime::Duration,
}

impl PlatformList {
    pub fn run(self) -> Result<Vec<PlatformInfo>> {
        let PlatformList { timeout } = self;
        let context = GlobalContext::default();
        let candidates = wasefire_protocol_usb::list(&context)?;
        println!("There are {} connected platforms:", candidates.len());
        let mut platforms = Vec::new();
        for candidate in candidates {
            let connection = candidate.connect(*timeout)?;
            platforms.push(connection.call::<service::PlatformInfo>(())?);
        }
        Ok(platforms)
    }
}

/// Reboots a platform.
#[derive(clap::Args)]
pub struct PlatformReboot {}

impl PlatformReboot {
    pub fn run<T: UsbContext>(self, connection: &Connection<T>) -> Result<()> {
        let PlatformReboot {} = self;
        connection.send(&Api::PlatformReboot(()))?;
        match connection.receive::<service::PlatformReboot>() {
            Ok(x) => *x.get(),
            Err(e) => match e.downcast_ref::<rusb::Error>() {
                Some(rusb::Error::Timeout | rusb::Error::NoDevice) => Ok(()),
                _ => Err(e),
            },
        }
    }
}

/// Calls a vendor RPC on a platform.
#[derive(clap::Args)]
pub struct PlatformRpc {
    #[clap(flatten)]
    rpc: Rpc,
}

impl PlatformRpc {
    pub fn run<T: UsbContext>(self, connection: &Connection<T>) -> Result<()> {
        let PlatformRpc { rpc } = self;
        rpc.write(connection.call::<service::PlatformVendor>(&rpc.read()?)?.get())
    }
}

/// Creates a new Rust applet project.
#[derive(clap::Args)]
pub struct RustAppletNew {
    /// Where to create the applet project.
    #[arg(value_hint = ValueHint::AnyPath)]
    path: PathBuf,

    /// Name of the applet project (defaults to the directory name).
    #[arg(long)]
    name: Option<String>,
}

impl RustAppletNew {
    pub fn run(&self) -> Result<()> {
        let RustAppletNew { path, name } = self;
        let mut cargo = Command::new("cargo");
        cargo.args(["new", "--lib"]).arg(path);
        if let Some(name) = name {
            cargo.arg(format!("--name={name}"));
        }
        cmd::execute(&mut cargo)?;
        cmd::execute(Command::new("cargo").args(["add", "wasefire"]).current_dir(path))?;
        let mut cargo = Command::new("cargo");
        cargo.args(["add", "wasefire-stub", "--optional"]);
        cmd::execute(cargo.current_dir(path))?;
        let mut sed = Command::new("sed");
        sed.arg("-i");
        sed.arg("s#^wasefire-stub\\( = .\"dep:wasefire-stub\"\\)#test\\1, \"wasefire/test\"#");
        sed.arg("Cargo.toml");
        cmd::execute(sed.current_dir(path))?;
        std::fs::remove_file(path.join("src/lib.rs"))?;
        fs::write(path.join("src/lib.rs"), include_str!("data/lib.rs"))?;
        Ok(())
    }
}

/// Builds a Rust applet from its project.
#[derive(Default, clap::Args)]
pub struct RustAppletBuild {
    /// Builds for production, disabling debugging facilities.
    #[arg(long)]
    pub prod: bool,

    /// Builds a native applet, e.g. --native=thumbv7em-none-eabi.
    #[arg(long, value_name = "TARGET")]
    pub native: Option<String>,

    /// Copies the final artifacts to this directory instead of target/wasefire.
    #[arg(long, value_name = "DIR", value_hint = ValueHint::DirPath)]
    pub output: Option<PathBuf>,

    /// Cargo profile, defaults to release.
    #[arg(long)]
    pub profile: Option<String>,

    /// Optimization level.
    #[clap(long, short = 'O')]
    pub opt_level: Option<OptLevel>,

    /// Stack size (ignored for native applets).
    #[clap(long, default_value = "16384")]
    pub stack_size: usize,

    /// Extra arguments to cargo, e.g. --features=foo.
    #[clap(last = true)]
    pub cargo: Vec<String>,
}

impl RustAppletBuild {
    pub fn run(&self, dir: impl AsRef<Path>) -> Result<()> {
        let metadata = metadata(dir.as_ref())?;
        let package = &metadata.packages[0];
        let target_dir = fs::try_relative(std::env::current_dir()?, &metadata.target_directory)?;
        let name = package.name.replace('-', "_");
        let mut cargo = Command::new("cargo");
        let mut rustflags = Vec::new();
        cargo.args(["rustc", "--lib"]);
        // We deliberately don't use the provided profile for those configs because they don't
        // depend on user-provided options (as opposed to opt-level).
        cargo.arg("--config=profile.release.codegen-units=1");
        cargo.arg("--config=profile.release.lto=true");
        cargo.arg("--config=profile.release.panic=\"abort\"");
        match &self.native {
            None => {
                rustflags.push(format!("-C link-arg=-zstack-size={}", self.stack_size));
                rustflags.push("-C target-feature=+bulk-memory".to_string());
                cargo.args(["--crate-type=cdylib", "--target=wasm32-unknown-unknown"]);
                wasefire_feature(package, "wasm", &mut cargo)?;
            }
            Some(target) => {
                cargo.args(["--crate-type=staticlib", &format!("--target={target}")]);
                wasefire_feature(package, "native", &mut cargo)?;
            }
        }
        let profile = self.profile.as_deref().unwrap_or("release");
        cargo.arg(format!("--profile={profile}"));
        if let Some(level) = self.opt_level {
            cargo.arg(format!("--config=profile.{profile}.opt-level={level}"));
        }
        cargo.args(&self.cargo);
        if self.prod {
            cargo.arg("-Zbuild-std=core,alloc");
            let mut features = "-Zbuild-std-features=panic_immediate_abort".to_string();
            if self.opt_level.map_or(false, OptLevel::optimize_for_size) {
                features.push_str(",optimize_for_size");
            }
            cargo.arg(features);
        } else {
            cargo.env("WASEFIRE_DEBUG", "");
        }
        cargo.env("RUSTFLAGS", rustflags.join(" "));
        cargo.current_dir(dir);
        cmd::execute(&mut cargo)?;
        let out_dir = match &self.output {
            Some(x) => x.clone(),
            None => "target/wasefire".into(),
        };
        let (src, dst) = match &self.native {
            None => (format!("wasm32-unknown-unknown/{profile}/{name}.wasm"), "applet.wasm"),
            Some(target) => (format!("{target}/{profile}/lib{name}.a"), "libapplet.a"),
        };
        let applet = out_dir.join(dst);
        if fs::copy_if_changed(target_dir.join(src), &applet)? && dst.ends_with(".wasm") {
            optimize_wasm(&applet, self.opt_level)?;
        }
        Ok(())
    }
}

/// Runs the unit-tests of a Rust applet project.
#[derive(clap::Args)]
pub struct RustAppletTest {
    /// Extra arguments to cargo, e.g. --features=foo.
    #[clap(last = true)]
    cargo: Vec<String>,
}

impl RustAppletTest {
    pub fn run(&self, dir: impl AsRef<Path>) -> Result<()> {
        let metadata = metadata(dir.as_ref())?;
        let package = &metadata.packages[0];
        ensure!(package.features.contains_key("test"), "missing test feature");
        let mut cargo = Command::new("cargo");
        cargo.args(["test", "--features=test"]);
        cargo.args(&self.cargo);
        cmd::replace(cargo)
    }
}

#[derive(Copy, Clone, ValueEnum)]
pub enum OptLevel {
    #[value(name = "0")]
    O0,
    #[value(name = "1")]
    O1,
    #[value(name = "2")]
    O2,
    #[value(name = "3")]
    O3,
    #[value(name = "s")]
    Os,
    #[value(name = "z")]
    Oz,
}

impl OptLevel {
    /// Returns whether the opt-level optimizes for size.
    pub fn optimize_for_size(self) -> bool {
        matches!(self, OptLevel::Os | OptLevel::Oz)
    }
}

impl Display for OptLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let value = self.to_possible_value().unwrap();
        let name = value.get_name();
        if f.alternate() || !matches!(self, OptLevel::Os | OptLevel::Oz) {
            write!(f, "{name}")
        } else {
            write!(f, "{name:?}")
        }
    }
}

/// Strips and optimizes a WASM applet.
pub fn optimize_wasm(applet: impl AsRef<Path>, opt_level: Option<OptLevel>) -> Result<()> {
    let mut strip = Command::new("wasm-strip");
    strip.arg(applet.as_ref());
    cmd::execute(&mut strip)?;
    let mut opt = Command::new("wasm-opt");
    opt.args(["--enable-bulk-memory", "--enable-sign-ext", "--enable-mutable-globals"]);
    match opt_level {
        Some(level) => drop(opt.arg(format!("-O{level:#}"))),
        None => drop(opt.arg("-O")),
    }
    opt.arg(applet.as_ref());
    opt.arg("-o");
    opt.arg(applet.as_ref());
    cmd::execute(&mut opt)?;
    Ok(())
}

fn metadata(dir: impl Into<PathBuf>) -> Result<Metadata> {
    let metadata = MetadataCommand::new().current_dir(dir).no_deps().exec()?;
    ensure!(metadata.packages.len() == 1, "not exactly one package");
    Ok(metadata)
}

fn wasefire_feature(
    package: &cargo_metadata::Package, feature: &str, cargo: &mut Command,
) -> Result<()> {
    if package.features.contains_key(feature) {
        cargo.arg(format!("--features={feature}"));
    } else {
        ensure!(
            package.dependencies.iter().any(|x| x.name == "wasefire"),
            "wasefire must be a direct dependency"
        );
        cargo.arg(format!("--features=wasefire/{feature}"));
    }
    Ok(())
}

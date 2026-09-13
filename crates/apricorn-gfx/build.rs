//! Generates the pinned NitroSDK fx16 sine/cosine table at build time.

use std::fmt::Write as _;
use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let mut source = String::from("[");
    for index in 0..4096 {
        let angle = index as f64 * std::f64::consts::TAU / 4096.0;
        let sin = (angle.sin() * 4096.0).round() as i16;
        let cos = (angle.cos() * 4096.0).round() as i16;
        write!(source, "[{sin},{cos}],").expect("writing a String cannot fail");
    }
    source.push(']');
    let output = PathBuf::from(std::env::var_os("OUT_DIR").expect("Cargo sets OUT_DIR"))
        .join("fx_sincos.rs");
    std::fs::write(output, source).expect("write generated SDK sine table");
}

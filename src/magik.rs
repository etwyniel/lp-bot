use std::process;

use anyhow::bail;

const RES: u64 = 350;

pub fn magik(url: &str, scale: f64) -> anyhow::Result<Vec<u8>> {
    let res = format!("{RES}x{RES}");
    let temp_res = ((RES as f64) * scale) as u64;
    dbg!(temp_res);
    let temp_res = format!("{temp_res}x{temp_res}");
    let out = process::Command::new("convert")
        .args([
            url,
            "-geometry",
            &res,
            "-liquid-rescale",
            &temp_res,
            "-liquid-rescale",
            &temp_res,
            "png:-",
        ])
        .output()?;
    if !out.status.success() {
        eprintln!(
            "Error processing image: {}",
            std::str::from_utf8(&out.stderr).unwrap_or("failed to get output")
        );
        bail!("Error processing image");
    }
    Ok(out.stdout)
}

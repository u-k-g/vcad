use std::path::PathBuf;

use vcad_kernel_reconstruct::{reconstruct, ReconstructionOptions};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = std::env::args_os().skip(1);
    let source = PathBuf::from(
        arguments
            .next()
            .ok_or("usage: reconstruct SOURCE [OUTPUT]")?,
    );
    let output = arguments.next().map(PathBuf::from);
    let data = std::fs::read(&source)?;
    let name = source
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or("source filename is not UTF-8")?;
    let result = reconstruct(&data, name, ReconstructionOptions::default())?;
    if let Some(output) = output {
        std::fs::write(output, &result.loon_source)?;
    } else {
        print!("{}", result.loon_source);
    }
    eprintln!("{}", serde_json::to_string_pretty(&result.report)?);
    Ok(())
}

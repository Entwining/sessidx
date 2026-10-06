use anyhow::Result;
use sessidx::{adapters, model::State, store::read_record};
use std::{
    fs::File,
    io::{BufReader, Read},
};

fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().collect();
    anyhow::ensure!(args.len() == 3, "profile_stream MANIFEST read|json|adapter");
    let manifest: serde_json::Value = serde_json::from_reader(File::open(&args[1])?)?;
    let mut last_peak = 0;
    for f in manifest.as_array().unwrap() {
        let mut reader = BufReader::new(
            File::open(f["path"].as_str().unwrap())?.take(f["size"].as_u64().unwrap()),
        );
        let mut buffer = Vec::new();
        let mut line = 0;
        let mut state = State::default();
        loop {
            let (length, complete, oversized) = read_record(&mut reader, &mut buffer)?;
            if !complete || length == 0 {
                break;
            }
            line += 1;
            if !oversized && args[2] != "read" {
                let parsed = if matches!(args[2].as_str(), "adapter" | "index_json") {
                    sessidx::normalize::record_for_index(&buffer, f["harness"].as_str().unwrap())
                } else {
                    serde_json::from_slice::<serde_json::Value>(&buffer)
                };
                if let Ok(v) = parsed {
                    if args[2] == "adapter" {
                        let _ = adapters::parse(f["harness"].as_str().unwrap(), &v, &mut state);
                    }
                }
            }
            let mut r = std::mem::MaybeUninit::<libc::rusage>::uninit();
            let peak = unsafe {
                anyhow::ensure!(
                    libc::getrusage(libc::RUSAGE_SELF, r.as_mut_ptr()) == 0,
                    "getrusage failed"
                );
                r.assume_init().ru_maxrss
            };
            if peak - last_peak > 10 * 1024 * 1024 {
                println!(
                    "{}",
                    serde_json::json!({"path":f["path"],"line":line,"bytes":length,"oversized":oversized,"buffer_capacity":buffer.capacity(),"peak_rss_bytes":peak,"phase":args[2]})
                );
                last_peak = peak;
            }
        }
    }
    Ok(())
}

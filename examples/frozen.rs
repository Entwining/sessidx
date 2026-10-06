use anyhow::Result;
use sessidx::store::Store;
use sha2::{Digest, Sha256};
use std::{fs::File, io::Read, path::Path};

fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().collect();
    anyhow::ensure!(args.len() == 3, "frozen SAMPLE_JSON DB_PATH");
    let manifest: serde_json::Value = serde_json::from_reader(File::open(&args[1])?)?;
    let mut store = Store::open(Path::new(&args[2]))?;
    for f in manifest["files"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("manifest files missing"))?
    {
        let path = Path::new(f["path"].as_str().unwrap());
        let size = f["size_bytes"].as_u64().unwrap();
        let mut reader = File::open(path)?.take(size);
        let mut hasher = Sha256::new();
        let mut buf = [0; 65536];
        let mut read = 0;
        loop {
            let n = reader.read(&mut buf)?;
            if n == 0 {
                break;
            }
            read += n as u64;
            hasher.update(&buf[..n]);
        }
        anyhow::ensure!(
            read == size && format!("{:x}", hasher.finalize()) == f["sha256"].as_str().unwrap(),
            "frozen prefix changed"
        );
        store.index_frozen(f["harness"].as_str().unwrap(), path, size)?;
    }
    println!("{}", sessidx::counting::doctor(&store.db)?);
    Ok(())
}

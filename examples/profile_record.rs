use anyhow::Result;
use sessidx::{adapters, model::State, normalize, shell};
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
};

fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().collect();
    anyhow::ensure!(
        args.len() == 6,
        "profile_record HARNESS FILE OFFSET LENGTH json|shell|all"
    );
    let mut file = File::open(&args[2])?;
    file.seek(SeekFrom::Start(args[3].parse()?))?;
    let length: usize = args[4].parse()?;
    anyhow::ensure!(length <= sessidx::store::MAX_RECORD, "record too large");
    let mut bytes = vec![0; length];
    file.read_exact(&mut bytes)?;
    let v: serde_json::Value = serde_json::from_slice(&bytes)?;
    let mut sites = 0;
    let mut events = 0;
    match args[5].as_str() {
        "json" => {}
        "all" => {
            let r = adapters::parse(&args[1], &v, &mut State::default());
            events = r.events.len();
            sites = r.events.iter().map(|e| e.sites.len()).sum();
        }
        "shell" => {
            let mut calls = Vec::new();
            if args[1] == "codex" {
                let p = &v["payload"];
                calls.push((
                    p["name"].as_str().unwrap_or(""),
                    normalize::arguments(&p["arguments"]),
                ));
            } else if let Some(blocks) = v
                .pointer("/message/content")
                .and_then(serde_json::Value::as_array)
            {
                for b in blocks {
                    calls.push((
                        b["name"].as_str().unwrap_or(""),
                        normalize::arguments(
                            b.get("input")
                                .or_else(|| b.get("arguments"))
                                .unwrap_or(&serde_json::Value::Null),
                        ),
                    ));
                }
            }
            for (name, arguments) in calls {
                if let Some(command) = normalize::shell(name, &arguments) {
                    sites += shell::sites(&command).len();
                }
            }
        }
        _ => anyhow::bail!("unknown phase"),
    }
    println!("{}", serde_json::json!({"events":events,"sites":sites}));
    Ok(())
}

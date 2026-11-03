use crate::common::{serial, sessidx, shell, stream};
use std::{fs, os::unix::fs::symlink, path::Path, process::Output};

const SKILL: &str = include_str!("../../skills/sessidx/SKILL.md");

/// A package prefix laid out as Homebrew links one: `bin/sessidx` a link to
/// the built binary, and the skill under `share/`.
fn package(dir: &Path) -> std::path::PathBuf {
    let prefix = dir.join("prefix");
    fs::create_dir_all(prefix.join("bin")).unwrap();
    symlink(env!("CARGO_BIN_EXE_sessidx"), prefix.join("bin/sessidx")).unwrap();
    let skill = prefix.join("share/sessidx/skills/sessidx");
    fs::create_dir_all(&skill).unwrap();
    fs::write(skill.join("SKILL.md"), SKILL).unwrap();
    prefix
}

fn install(binary: &Path, home: &Path) -> Output {
    let guard = serial();
    shell(&guard, r#"exec "$0" skill install"#)
        .arg(binary)
        .env("HOME", home)
        .output()
        .unwrap()
}

#[test]
fn skill_show_prints_the_shipped_skill() {
    let dir = tempfile::tempdir().unwrap();
    let guard = serial();
    let out = sessidx(
        &guard,
        &dir.path().join("index.db"),
        &[],
        &["skill", "show"],
    )
    .output()
    .unwrap();
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(String::from_utf8(out.stdout).unwrap(), SKILL);
}

#[test]
fn skill_install_links_both_directories_through_the_unresolved_prefix() {
    let dir = tempfile::tempdir().unwrap();
    let prefix = package(dir.path());
    let source = prefix.join("share/sessidx/skills/sessidx");
    let home = dir.path().join("home");
    fs::create_dir_all(home.join(".agents/skills")).unwrap();
    symlink("/elsewhere", home.join(".agents/skills/sessidx")).unwrap();

    let records = stream(&install(&prefix.join("bin/sessidx"), &home), 0);
    for dir in [".claude/skills", ".agents/skills"] {
        let link = home.join(dir).join("sessidx");
        assert_eq!(fs::read_link(&link).unwrap(), source);
        assert!(records.iter().any(|r| r["type"] == "link"
            && r["path"] == link.to_str().unwrap()
            && r["target"] == source.to_str().unwrap()));
    }
    let end = records.last().unwrap();
    assert_eq!(end["complete"], true);
    assert_eq!(end["searched"]["returned"], 2);
}

#[test]
fn skill_install_accepts_claude_skills_linked_to_agents_skills() {
    let dir = tempfile::tempdir().unwrap();
    let prefix = package(dir.path());
    let home = dir.path().join("home");
    fs::create_dir_all(home.join(".agents/skills")).unwrap();
    fs::create_dir_all(home.join(".claude")).unwrap();
    symlink("../.agents/skills", home.join(".claude/skills")).unwrap();

    stream(&install(&prefix.join("bin/sessidx"), &home), 0);
    assert_eq!(
        fs::read_link(home.join(".agents/skills/sessidx")).unwrap(),
        prefix.join("share/sessidx/skills/sessidx")
    );
}

#[test]
fn skill_install_leaves_a_skill_directory_in_place() {
    let dir = tempfile::tempdir().unwrap();
    let prefix = package(dir.path());
    let home = dir.path().join("home");
    let copy = home.join(".claude/skills/sessidx");
    fs::create_dir_all(&copy).unwrap();
    fs::write(copy.join("SKILL.md"), "older copy").unwrap();

    let records = stream(&install(&prefix.join("bin/sessidx"), &home), 2);
    let error = records[0]["error"].as_str().unwrap();
    assert!(error.contains(copy.to_str().unwrap()), "{error}");
    assert_eq!(
        fs::read_to_string(copy.join("SKILL.md")).unwrap(),
        "older copy"
    );
}

#[test]
fn skill_install_fails_without_a_packaged_skill() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    let records = stream(&install(Path::new(env!("CARGO_BIN_EXE_sessidx")), &home), 2);
    assert!(
        records[0]["error"]
            .as_str()
            .unwrap()
            .contains("no skill is packaged")
    );
    assert!(!home.exists());
}
